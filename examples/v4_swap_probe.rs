//! HERRAMIENTA DE DIAGNÓSTICO (no forma parte del binario; conservar).
//! Escrita en el Paso 0 (2026-09-14) y documentada en CLAUDE.md, sección
//! "Paso 0". Sirve para re-verificar en cualquier momento:
//!   - topic0 de todos los eventos (curva, factory, hook, PoolManager V4),
//!   - los PoolGraduated reales de la factory de Pons V2,
//!   - la derivación PoolKey → PoolId de un token graduado y su cruce con
//!     PoolRegistered (hook) e Initialize (PoolManager),
//!   - los Swap V4 de ese pool y que el último coincide con slot0 (StateView).
//!
//!   cargo run --example v4_swap_probe
//!   PROBE_TOKEN=0x... cargo run --example v4_swap_probe   # otro token graduado
//!   PROBE_SKIP_SURVEY=1  salta el muestreo de pairTokens (ahorra ~40 calls)
//!   PROBE_RPC_URL=...    RPC para eth_getLogs (por defecto el público oficial;
//!                        Alchemy Free no sirve: 10 bloques máximo)
//!   PROBE_CHUNK=N        tamaño de rango por getLogs de Swap (default 5M)
//!
//! Provider dual: si hay ALCHEMY_API_KEY en .env, los eth_call van por
//! Alchemy y solo los getLogs por el público (que devuelve 429 en ráfagas
//! de eth_call y "log query timed out" en rangos enormes).
use alloy::primitives::{keccak256, Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{BlockNumberOrTag, Filter};
use alloy::sol;
use alloy::sol_types::{SolEvent, SolValue};

sol! {
    // Uniswap v4-core src/interfaces/IPoolManager.sol (leído 2026-09-14)
    #[derive(Debug)]
    event Swap(bytes32 indexed id, address indexed sender, int128 amount0, int128 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick, uint24 fee);
    #[derive(Debug)]
    event Initialize(bytes32 indexed id, address indexed currency0, address indexed currency1, uint24 fee, int24 tickSpacing, address hooks, uint160 sqrtPriceX96, int24 tick);
    // Pons V2 factory / hook / curva (contractsV2/src/v2)
    #[derive(Debug)]
    event PoolGraduated(address indexed token, uint256 positionId, uint256 tokenAmount, uint256 pairTokenAmount);
    #[derive(Debug)]
    event TokenLaunched(address indexed token, address indexed curve, address indexed deployer, address pairToken, uint256 launchConfigId, uint256 graduationThreshold);
    #[derive(Debug)]
    event PoolRegistered(bytes32 indexed poolId, address memecoin, address quoteToken, address creator);
    #[derive(Debug)]
    event CurveBuy(address indexed buyer, address indexed recipient, uint256 quoteIn, uint256 tokensOut, uint256 fee, uint256 tax);
    #[derive(Debug)]
    event CurveSell(address indexed seller, address indexed recipient, uint256 tokensIn, uint256 quoteOut, uint256 fee, uint256 tax);

    struct PoolKey { address currency0; address currency1; uint24 fee; int24 tickSpacing; address hooks; }

    struct LaunchedToken {
        address token; address curve; address deployer; address creatorFeeRecipient; address pairToken;
        uint256 graduationThreshold; uint24 poolFee; int24 tickSpacing; uint16 creatorTaxBps; bool buybackEnabled;
        uint8 phase; uint256 sweptQuote; uint256 sweptTokens; uint256 sweptAt; bool exists;
    }
    #[sol(rpc)]
    interface IFactory { function getLaunchedToken(address token) external view returns (LaunchedToken memory); }
    #[sol(rpc)]
    interface IStateView {
        function poolManager() external view returns (address);
        function getSlot0(bytes32 poolId) external view returns (uint160 sqrtPriceX96, int24 tick, uint24 protocolFee, uint24 lpFee);
        function getLiquidity(bytes32 poolId) external view returns (uint128);
    }
    #[sol(rpc)]
    interface IERC20 { function decimals() external view returns (uint8); function symbol() external view returns (string); function totalSupply() external view returns (uint256); }
}

const FACTORY: &str = "0x7ed598bcef8bd9edd8c97a195c6d13f40801ec7e";
const HOOK: &str = "0xe5e702641ea86f4ae6cc3cdaed2b886f976be044";
const POOL_MANAGER: &str = "0x8366a39cc670b4001a1121b8f6a443a643e40951";
const STATE_VIEW: &str = "0xf3334192d15450cdd385c8b70e03f9a6bd9e673b";
const FACTORY_START: u64 = 8991118;

fn price_from_sqrt(sqrt: U256) -> f64 {
    // price1/0 = (sqrtPriceX96 / 2^96)^2, en unidades crudas (sin decimals)
    let s: f64 = sqrt.to_string().parse::<f64>().unwrap();
    let r = s / 2f64.powi(96);
    r * r
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    println!("== topic0 recomputados con keccak ==");
    for (name, sig) in [
        ("Swap(V4)", "Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)"),
        ("Initialize(V4)", "Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)"),
        ("PoolGraduated", "PoolGraduated(address,uint256,uint256,uint256)"),
        ("TokenLaunched", "TokenLaunched(address,address,address,address,uint256,uint256)"),
        ("LaunchSwept", "LaunchSwept(address,uint256,uint256)"),
        ("PoolRegistered", "PoolRegistered(bytes32,address,address,address)"),
        ("CurveBuy", "CurveBuy(address,address,uint256,uint256,uint256,uint256)"),
        ("CurveSell", "CurveSell(address,address,uint256,uint256,uint256,uint256)"),
        ("CurveBuyRefunded", "CurveBuyRefunded(address,uint256)"),
        ("FeesSwept", "FeesSwept(uint256,uint256,uint256)"),
        ("FeesRescued", "FeesRescued(address,address,uint256,uint256)"),
        ("BuybackLocked", "BuybackLocked(uint256,uint256)"),
        ("CurveCompleted", "CurveCompleted(address,uint256,uint256)"),
        ("Initialized", "Initialized(address)"),
        ("CreatorFeeRecipientUpdated", "CreatorFeeRecipientUpdated(address,address)"),
        ("BuybackEnabledUpdated", "BuybackEnabledUpdated(bool)"),
        ("AutoGraduationFailed", "AutoGraduationFailed(address,uint256)"),
        ("LaunchForceSwept", "LaunchForceSwept(address)"),
        ("HookFeeCollected", "HookFeeCollected(bytes32,address,uint256,uint256)"),
    ] {
        println!("{name:16} {sig}\n{:16} {}", "", keccak256(sig.as_bytes()));
    }
    assert_eq!(Swap::SIGNATURE_HASH, keccak256(b"Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)"));

    // Alchemy Free tier limita eth_getLogs a 10 bloques; el RPC público
    // oficial sí sirve rangos completos con address+topic (comprobado 2026-09-14).
    // Provider dual (mismo diseño que el Paso 1): `logs` = RPC público para
    // eth_getLogs de rango amplio; `provider` (calls) = Alchemy si hay
    // ALCHEMY_API_KEY, si no el público también. Nunca se imprime la URL.
    let public_url = std::env::var("PROBE_RPC_URL").unwrap_or_else(|_| "https://rpc.mainnet.chain.robinhood.com".to_string());
    let logs = ProviderBuilder::new().connect_http(public_url.parse()?);
    let provider = match std::env::var("ALCHEMY_API_KEY") {
        Ok(k) if !k.is_empty() => { println!("calls: Alchemy; logs: RPC público"); ProviderBuilder::new().connect_http(format!("https://robinhood-mainnet.g.alchemy.com/v2/{k}").parse()?) }
        _ => { println!("calls y logs: RPC público"); logs.clone() }
    };
    let chain_id = provider.get_chain_id().await?;
    anyhow::ensure!(logs.get_chain_id().await? == chain_id, "chain_id distinto entre providers");
    let latest = provider.get_block_number().await?;
    println!("\nchain_id={chain_id} latest_block={latest}");

    let factory: Address = FACTORY.parse()?;
    let hook: Address = HOOK.parse()?;
    let pm: Address = POOL_MANAGER.parse()?;
    let sv: Address = STATE_VIEW.parse()?;

    // 1) PoolGraduated reales
    let f = Filter::new().address(factory).event_signature(PoolGraduated::SIGNATURE_HASH)
        .from_block(FACTORY_START).to_block(BlockNumberOrTag::Latest);
    let grads = logs.get_logs(&f).await?;
    println!("\n== PoolGraduated en factory desde {FACTORY_START}: {} ==", grads.len());
    let mut decoded = vec![];
    for l in &grads {
        let d = PoolGraduated::decode_log(&l.inner)?;
        if decoded.len() < 3 || decoded.len() + 3 >= grads.len() {
            println!("block {:?} tx {:?} token {} positionId {} tokenAmount {} pairTokenAmount {}",
                l.block_number, l.transaction_hash, d.token, d.positionId, d.tokenAmount, d.pairTokenAmount);
        }
        decoded.push((l.clone(), d));
    }
    // resumen de pairTokens de todas las graduaciones
    let fac = IFactory::new(factory, &provider);
    let mut pair_counts: std::collections::BTreeMap<Address, usize> = Default::default();
    let n = decoded.len();
    let skip_survey = std::env::var("PROBE_SKIP_SURVEY").is_ok();
    let sample: Vec<_> = decoded.iter().enumerate().filter(|(i, _)| !skip_survey && (*i < 10 || *i + 10 >= n)).map(|(_, x)| x).collect();
    for (_, d) in &sample {
        let lt = fac.getLaunchedToken(d.token).call().await?;
        *pair_counts.entry(lt.pairToken).or_default() += 1;
    }
    println!("\n== pairToken de una muestra de {} de las {} graduaciones (10 primeras + 10 últimas) ==", sample.len(), n);
    for (p, n) in &pair_counts {
        let (sym, dec) = if *p == Address::ZERO { ("ETH nativo".to_string(), 18) } else {
            (IERC20::new(*p, &provider).symbol().call().await.unwrap_or("?".into()), IERC20::new(*p, &provider).decimals().call().await.unwrap_or(0))
        };
        println!("  {p}  symbol={sym} decimals={dec}  n={n}");
    }
    let (glog, g) = match std::env::var("PROBE_TOKEN") {
        Ok(t) => { let t: Address = t.parse()?; decoded.iter().find(|(_, d)| d.token == t).cloned().ok_or_else(|| anyhow::anyhow!("PROBE_TOKEN no graduado"))? }
        Err(_) => decoded.first().cloned().ok_or_else(|| anyhow::anyhow!("no hay PoolGraduated"))?,
    };
    let (glog, g) = (&glog, &g);
    let token = g.token;
    println!("\n>> token de validación: {token} (primer PoolGraduated, tx {:?}, block {:?})", glog.transaction_hash, glog.block_number);

    // 2) getLaunchedToken → PoolKey → PoolId
    let lt = fac.getLaunchedToken(token).call().await?;
    println!("getLaunchedToken: curve={} pairToken={} poolFee={} tickSpacing={} creatorTaxBps={} phase={} threshold={} sweptQuote={} sweptTokens={} exists={}",
        lt.curve, lt.pairToken, lt.poolFee, lt.tickSpacing, lt.creatorTaxBps, lt.phase, lt.graduationThreshold, lt.sweptQuote, lt.sweptTokens, lt.exists);
    let (c0, c1, meme_is_0) = if lt.pairToken < token { (lt.pairToken, token, false) } else { (token, lt.pairToken, true) };
    let pk = PoolKey { currency0: c0, currency1: c1, fee: lt.poolFee, tickSpacing: lt.tickSpacing, hooks: hook };
    let pool_id = keccak256(pk.abi_encode());
    println!("PoolKey: currency0={c0} currency1={c1} fee={} tickSpacing={} hooks={hook}  memecoinIsCurrency0={meme_is_0}", lt.poolFee, lt.tickSpacing);
    println!("PoolId (keccak(abi.encode(PoolKey))) = {pool_id}");

    // 3) cross-check con PoolRegistered del hook e Initialize del PoolManager en la misma tx
    let txh = glog.transaction_hash.unwrap();
    let rcpt = provider.get_transaction_receipt(txh).await?.unwrap();
    let mut seen_reg = false; let mut seen_init = false;
    for l in rcpt.inner.logs() {
        if l.address() == hook && l.topics().first() == Some(&PoolRegistered::SIGNATURE_HASH) {
            let d = PoolRegistered::decode_log(&l.inner)?;
            println!("PoolRegistered(hook): poolId={} memecoin={} quoteToken={} creator={}  match={}", d.poolId, d.memecoin, d.quoteToken, d.creator, d.poolId == pool_id);
            seen_reg = true;
        }
        if l.address() == pm && l.topics().first() == Some(&Initialize::SIGNATURE_HASH) {
            let d = Initialize::decode_log(&l.inner)?;
            println!("Initialize(PoolManager): id={} c0={} c1={} fee={} ts={} hooks={} sqrtP={} tick={}  match={}", d.id, d.currency0, d.currency1, d.fee, d.tickSpacing, d.hooks, d.sqrtPriceX96, d.tick, d.id == pool_id);
            seen_init = true;
        }
    }
    println!("cross-check: PoolRegistered visto={seen_reg} Initialize visto={seen_init}");

    // 4) Swaps del PoolManager para ese PoolId
    // Troceado: el RPC público agota el tiempo ("log query timed out") con
    // rangos de decenas de millones de bloques para un PoolId con pocos logs.
    let chunk: u64 = std::env::var("PROBE_CHUNK").ok().and_then(|v| v.parse().ok()).unwrap_or(5_000_000);
    let mut swaps = vec![];
    let mut from = glog.block_number.unwrap();
    while from <= latest {
        let to = (from + chunk - 1).min(latest);
        let f = Filter::new().address(pm).event_signature(Swap::SIGNATURE_HASH).topic1(pool_id).from_block(from).to_block(to);
        let mut attempt = 0;
        let part = loop {
            match logs.get_logs(&f).await {
                Ok(v) => break v,
                Err(e) if attempt < 3 => { attempt += 1; eprintln!("getLogs {from}-{to} falló ({e}); reintento {attempt}"); tokio::time::sleep(std::time::Duration::from_secs(3 * attempt)).await; }
                Err(e) => return Err(e.into()),
            }
        };
        println!("  getLogs Swap {from}-{to}: {} logs", part.len());
        swaps.extend(part);
        from = to + 1;
    }
    println!("\n== Swap(V4) del PoolManager con topic1=PoolId: {} logs ==", swaps.len());
    let dec = IERC20::new(token, &provider).decimals().call().await?;
    let sym = IERC20::new(token, &provider).symbol().call().await?;
    let supply = IERC20::new(token, &provider).totalSupply().call().await?;
    let pair_dec = if lt.pairToken == Address::ZERO { 18 } else { IERC20::new(lt.pairToken, &provider).decimals().call().await? };
    println!("token symbol={sym} decimals={dec} totalSupply={supply}  pairToken decimals={pair_dec} (ZERO => ETH nativo)");
    let mut last: Option<Swap> = None;
    for (i, l) in swaps.iter().enumerate() {
        let d = Swap::decode_log(&l.inner)?;
        if i < 5 || i + 3 >= swaps.len() {
            println!("#{i} block {:?} topics={} data_len={} sender={} amount0={} amount1={} sqrtP={} liq={} tick={} fee={}",
                l.block_number, l.topics().len(), l.data().data.len(), d.sender, d.amount0, d.amount1, d.sqrtPriceX96, d.liquidity, d.tick, d.fee);
            // precio derivado de sqrtPriceX96 vs de los amounts
            let p10 = price_from_sqrt(U256::from(d.sqrtPriceX96)); // c1 por c0, crudo
            let a0: f64 = d.amount0.to_string().parse().unwrap();
            let a1: f64 = d.amount1.to_string().parse().unwrap();
            let p_amounts = (a1 / a0).abs();
            let (meme_per_pair_raw, label) = if meme_is_0 { (p10, "pair por 1 meme (crudo)") } else { (1.0 / p10, "pair por 1 meme (crudo)") };
            let scale = 10f64.powi(dec as i32 - pair_dec as i32);
            let meme_price = meme_per_pair_raw * scale;
            let from_amounts = (if meme_is_0 { p_amounts } else { 1.0 / p_amounts }) * scale;
            println!("     {label}: sqrtP→{meme_price:.12}  amounts→{from_amounts:.12}   (memecoinIsCurrency0={meme_is_0})");
        }
        last = Some(d.data);
    }

    // 5) slot0 vía StateView (documental) — también verifica su bytecode y poolManager()
    let code = provider.get_code_at(sv).await?;
    let svc = IStateView::new(sv, &provider);
    let sv_pm = svc.poolManager().call().await?;
    println!("\nStateView {sv}: bytecode={} poolManager()={sv_pm} == PoolManager? {}", !code.is_empty(), sv_pm == pm);
    let s0 = svc.getSlot0(pool_id).call().await?;
    let liq = svc.getLiquidity(pool_id).call().await?;
    println!("slot0: sqrtPriceX96={} tick={} protocolFee={} lpFee={} liquidity={liq}", s0.sqrtPriceX96, s0.tick, s0.protocolFee, s0.lpFee);
    if let Some(l) = last {
        println!("último Swap: sqrtPriceX96={} tick={} liq={}  == slot0? {}", l.sqrtPriceX96, l.tick, l.liquidity, l.sqrtPriceX96 == s0.sqrtPriceX96);
    }
    let p10 = price_from_sqrt(U256::from(s0.sqrtPriceX96));
    let scale = 10f64.powi(dec as i32 - pair_dec as i32);
    let meme_price = (if meme_is_0 { p10 } else { 1.0 / p10 }) * scale;
    let supply_f: f64 = supply.to_string().parse::<f64>().unwrap() / 10f64.powi(dec as i32);
    println!("precio actual (slot0): {meme_price:.12} pairToken por 1 {sym};  mcap(totalSupply)= {:.6} pairToken", meme_price * supply_f);
    Ok(())
}
