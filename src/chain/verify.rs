//! Verificación on-chain de direcciones antes de usarlas para tradear.
//!
//! Equivalente en Rust a lo que se haría manualmente con:
//!   cast code <address> --rpc-url <rpc>
//!   cast call <address> "factory()(address)" --rpc-url <rpc>   (o el getter que aplique)
//!
//! Esto NO es opcional ni "para producción más adelante" — las direcciones
//! que trae config.example.toml para Pons vienen de documentación de
//! terceros (Bitquery), no de una fuente on-chain verificada por este
//! propio código. El pool_manager de Uniswap V4 en el ejemplo ni siquiera
//! está completo (dirección truncada en la fuente original).

use alloy::primitives::Address;
use alloy::providers::{DynProvider, Provider};

#[derive(Debug)]
pub struct ContractCheckResult {
    pub address: String,
    pub has_bytecode: bool,
    /// Resultado de una llamada de lectura esperada (ej. factory() en un
    /// router, o el selector correspondiente) — None si no se pudo verificar
    /// el rol del contrato, solo que tiene bytecode desplegado.
    pub role_confirmed: bool,
    pub notes: Vec<String>,
}

/// Verifica que una dirección tenga bytecode desplegado. El primer filtro
/// más básico: si esto falla, la dirección está mal o aún no se ha
/// desplegado nada ahí — no seguir con nada más.
pub async fn has_bytecode(provider: &DynProvider, address: &str) -> anyhow::Result<bool> {
    let addr = parse_address(address)?;
    let code = provider
        .get_code_at(addr)
        .await
        .map_err(|e| anyhow::anyhow!("eth_getCode({address}) falló: {e}"))?;
    Ok(!code.is_empty())
}

fn parse_address(s: &str) -> anyhow::Result<Address> {
    s.parse::<Address>()
        .map_err(|e| anyhow::anyhow!("dirección inválida {s:?}: {e}"))
}

alloy::sol! {
    // Getters leídos del fuente oficial de Pons V2
    // (github.com/ponsdotdev/ponsfamily/contractsV2/src/v2/PonsV2LaunchFactory.sol
    // y hooks/PonsV2MemeHook.sol). NO añadir nombres que no estén ahí.
    #[sol(rpc)]
    interface IPonsV2LaunchFactory {
        function poolManager() external view returns (address);
        function positionManager() external view returns (address);
        function permit2() external view returns (address);
        function launchDeployer() external view returns (address);
        function launchForwarder() external view returns (address);
        function launchEnabled() external view returns (bool);
        function launchConfigCount() external view returns (uint256);
    }

    #[sol(rpc)]
    interface IPonsV2MemeHook {
        function factory() external view returns (address);
    }

    // Getters estándar del PoolManager de Uniswap V4 (Owned + IProtocolFees).
    #[sol(rpc)]
    interface IPoolManager {
        function owner() external view returns (address);
        function protocolFeeController() external view returns (address);
    }

    // StateView de Uniswap V4 (v4-periphery src/lens/StateView.sol):
    // `poolManager()` inmutable y lectura de slot0 por PoolId.
    #[sol(rpc)]
    interface IStateView {
        function poolManager() external view returns (address);
        function getSlot0(bytes32 poolId) external view returns (uint160 sqrtPriceX96, int24 tick, uint24 protocolFee, uint24 lpFee);
    }
}

impl ContractCheckResult {
    fn new(address: &str) -> Self {
        Self { address: address.to_string(), has_bytecode: false, role_confirmed: false, notes: vec![] }
    }
    fn note(&mut self, s: impl Into<String>) {
        self.notes.push(s.into());
    }
    pub fn verdict(&self) -> &'static str {
        match (self.has_bytecode, self.role_confirmed) {
            (true, true) => "bytecode confirmado y rol confirmado",
            (true, false) => "bytecode confirmado pero rol NO confirmado",
            (false, _) => "FALLO: sin bytecode",
        }
    }
}

fn same(a: Address, b: &str) -> bool {
    parse_address(b).map(|x| x == a).unwrap_or(false)
}

/// Verificación específica por rol de contrato. Cada contrato tiene getters
/// que confirman su rol real (no solo que "algo" está desplegado ahí).
///
/// Factory de Pons V2: debe responder a sus getters inmutables y su
/// `poolManager()` / `permit2()` deben coincidir con lo que dice config para
/// Uniswap V4 — verificación cruzada entre las dos fuentes.
pub async fn confirm_pons_launch_factory(
    provider: &DynProvider,
    address: &str,
    expected_pool_manager: &str,
    expected_permit2: &str,
) -> anyhow::Result<ContractCheckResult> {
    let mut r = ContractCheckResult::new(address);
    r.has_bytecode = has_bytecode(provider, address).await?;
    if !r.has_bytecode {
        return Ok(r);
    }
    let c = IPonsV2LaunchFactory::new(parse_address(address)?, provider);
    let pm = match c.poolManager().call().await {
        Ok(v) => v,
        Err(e) => { r.note(format!("poolManager() revirtió/falló: {e}")); return Ok(r); }
    };
    let permit2 = match c.permit2().call().await {
        Ok(v) => v,
        Err(e) => { r.note(format!("permit2() revirtió/falló: {e}")); return Ok(r); }
    };
    let pos = c.positionManager().call().await.map_err(|e| anyhow::anyhow!("positionManager(): {e}"))?;
    let count = c.launchConfigCount().call().await.map_err(|e| anyhow::anyhow!("launchConfigCount(): {e}"))?;
    let enabled = c.launchEnabled().call().await.map_err(|e| anyhow::anyhow!("launchEnabled(): {e}"))?;
    r.note(format!("poolManager() = {pm:#x}"));
    r.note(format!("positionManager() = {pos:#x}"));
    r.note(format!("permit2() = {permit2:#x}"));
    r.note(format!("launchConfigCount() = {count}, launchEnabled() = {enabled}"));
    let pm_ok = same(pm, expected_pool_manager);
    let p2_ok = same(permit2, expected_permit2);
    r.note(format!("poolManager() == config.uniswap_v4.pool_manager: {pm_ok}"));
    r.note(format!("permit2() == config.uniswap_v4.permit2: {p2_ok}"));
    r.role_confirmed = pm_ok && p2_ok;
    Ok(r)
}

/// Hook de graduación de Pons V2 (`PonsV2MemeHook`): su `factory()` debe
/// ser exactamente la factory de config.
pub async fn confirm_pons_meme_hook(
    provider: &DynProvider,
    address: &str,
    expected_factory: &str,
) -> anyhow::Result<ContractCheckResult> {
    let mut r = ContractCheckResult::new(address);
    r.has_bytecode = has_bytecode(provider, address).await?;
    if !r.has_bytecode {
        return Ok(r);
    }
    let c = IPonsV2MemeHook::new(parse_address(address)?, provider);
    match c.factory().call().await {
        Ok(f) => {
            r.note(format!("factory() = {f:#x}"));
            r.role_confirmed = same(f, expected_factory);
            r.note(format!("factory() == config launch_factory: {}", r.role_confirmed));
        }
        Err(e) => r.note(format!("factory() revirtió/falló: {e}")),
    }
    Ok(r)
}

/// La dirección etiquetada "router" en config viene de Bitquery; en el fuente
/// V2 no existe ningún contrato Router. Los únicos roles "de entrada" que la
/// factory conoce son `launchForwarder()` y `launchDeployer()`: el rol se
/// confirma solo si la dirección coincide con uno de ellos.
pub async fn confirm_pons_launch_router(
    provider: &DynProvider,
    address: &str,
    factory: &str,
) -> anyhow::Result<ContractCheckResult> {
    let mut r = ContractCheckResult::new(address);
    r.has_bytecode = has_bytecode(provider, address).await?;
    if !r.has_bytecode {
        return Ok(r);
    }
    let c = IPonsV2LaunchFactory::new(parse_address(factory)?, provider);
    let fwd = c.launchForwarder().call().await.map_err(|e| anyhow::anyhow!("launchForwarder(): {e}"))?;
    let dep = c.launchDeployer().call().await.map_err(|e| anyhow::anyhow!("launchDeployer(): {e}"))?;
    r.note(format!("factory.launchForwarder() = {fwd:#x}"));
    r.note(format!("factory.launchDeployer() = {dep:#x}"));
    if same(fwd, address) {
        r.role_confirmed = true;
        r.note("coincide con launchForwarder(): es el forwarder de lanzamientos, no un 'router'");
    } else if same(dep, address) {
        r.role_confirmed = true;
        r.note("coincide con launchDeployer(): es el deployer de lanzamientos, no un 'router'");
    } else {
        r.note("no coincide con ningún rol conocido de la factory V2 — origen: Bitquery, sin fuente oficial");
    }
    Ok(r)
}

/// Singleton de Uniswap V4: responde a `owner()`/`protocolFeeController()` y,
/// más fuerte, la factory de Pons declara esta misma dirección como su
/// `poolManager()`.
pub async fn confirm_uniswap_v4_pool_manager(
    provider: &DynProvider,
    address: &str,
    pons_factory: &str,
) -> anyhow::Result<ContractCheckResult> {
    let mut r = ContractCheckResult::new(address);
    r.has_bytecode = has_bytecode(provider, address).await?;
    if !r.has_bytecode {
        return Ok(r);
    }
    let c = IPoolManager::new(parse_address(address)?, provider);
    let owner = match c.owner().call().await {
        Ok(v) => v,
        Err(e) => { r.note(format!("owner() revirtió/falló: {e}")); return Ok(r); }
    };
    let pfc = match c.protocolFeeController().call().await {
        Ok(v) => v,
        Err(e) => { r.note(format!("protocolFeeController() revirtió/falló: {e}")); return Ok(r); }
    };
    r.note(format!("owner() = {owner:#x}"));
    r.note(format!("protocolFeeController() = {pfc:#x}"));
    let f = IPonsV2LaunchFactory::new(parse_address(pons_factory)?, provider);
    let pm = f.poolManager().call().await.map_err(|e| anyhow::anyhow!("factory.poolManager(): {e}"))?;
    r.role_confirmed = same(pm, address);
    r.note(format!("pons_factory.poolManager() == esta dirección: {}", r.role_confirmed));
    Ok(r)
}

/// StateView de Uniswap V4 (lens de solo lectura): su `poolManager()` debe
/// ser el PoolManager de config y, además, `getSlot0` del pool de JACKET
/// (token graduado de Pons V2, PoolId derivado y contrastado el 2026-09-14,
/// ver CLAUDE.md "Paso 0") debe devolver un precio distinto de cero.
pub async fn confirm_uniswap_v4_state_view(
    provider: &DynProvider,
    address: &str,
    expected_pool_manager: &str,
) -> anyhow::Result<ContractCheckResult> {
    let mut r = ContractCheckResult::new(address);
    r.has_bytecode = has_bytecode(provider, address).await?;
    if !r.has_bytecode {
        return Ok(r);
    }
    let c = IStateView::new(parse_address(address)?, provider);
    let pm = match c.poolManager().call().await {
        Ok(v) => v,
        Err(e) => { r.note(format!("poolManager() revirtió/falló: {e}")); return Ok(r); }
    };
    r.note(format!("poolManager() = {pm:#x}"));
    let pm_ok = same(pm, expected_pool_manager);
    r.note(format!("poolManager() == config.uniswap_v4.pool_manager: {pm_ok}"));
    let pool_id: alloy::primitives::B256 = JACKET_POOL_ID.parse()?;
    let slot0_ok = match c.getSlot0(pool_id).call().await {
        Ok(s) => {
            r.note(format!("getSlot0(JACKET) sqrtPriceX96 = {}, tick = {}, lpFee = {}", s.sqrtPriceX96, s.tick, s.lpFee));
            !s.sqrtPriceX96.is_zero()
        }
        Err(e) => { r.note(format!("getSlot0(JACKET) revirtió/falló: {e}")); false }
    };
    r.role_confirmed = pm_ok && slot0_ok;
    Ok(r)
}

/// Token JACKET: primer `PoolGraduated` de la factory V2 (bloque 27828161),
/// par NVDA. Caso de validación post-graduación. Ver CLAUDE.md "Paso 0".
pub const JACKET_TOKEN: &str = "0xc9e9ab90654f82893D7Fd18b62f694992E8CEF29";
/// PoolId de JACKET/NVDA = keccak256(abi.encode(PoolKey)), igual al
/// `PoolRegistered.poolId` del hook y al `Initialize.id` del PoolManager.
pub const JACKET_POOL_ID: &str = "0x6eb457f0729bd458608099505990f03d8a6af91202f936124f72ad76c96f6fe1";

/// Token PONS: publicado por la doc oficial de Pons como caso de prueba
/// para validar indexadores/integraciones contra estado on-chain conocido.
/// Graduado, lanzado por la factory legacy (V1). Ver CLAUDE.md.
pub const PONS_REFERENCE_TOKEN: &str = "0x39dBED3a2bd333467115dE45665cC57F813C4571";
/// Dirección sin bytecode (control negativo para `has_bytecode`).
const NEGATIVE_CONTROL: &str = "0x000000000000000000000000000000000000dEaD";

/// Punto de entrada de `cargo run -- verify`. Devuelve error (exit code ≠ 0)
/// si cualquier comprobación falla.
pub async fn run_verify_cli(cfg: &crate::config::AppConfig) -> anyhow::Result<()> {
    let provider = crate::chain::ChainProvider::connect(&cfg.chain).await?;
    let http = provider.http();
    println!("chain_id confirmado por eth_chainId: {}", provider.chain_id);

    let pons = has_bytecode(http, PONS_REFERENCE_TOKEN).await?;
    let neg = has_bytecode(http, NEGATIVE_CONTROL).await?;
    println!("has_bytecode({PONS_REFERENCE_TOKEN}) = {pons}   [token de referencia PONS, esperado true]");
    println!("has_bytecode({NEGATIVE_CONTROL}) = {neg}   [control negativo, esperado false]");
    if !pons || neg {
        anyhow::bail!("has_bytecode no se comporta como se espera contra el token de referencia");
    }

    let v4 = &cfg.uniswap_v4;
    let mut all_ok = true;
    for lp in cfg.launchpads.iter().filter(|l| l.enabled) {
        if lp.name != "pons" {
            println!("\n[{}] launchpad habilitado sin verificador implementado — NO verificado", lp.name);
            all_ok = false;
            continue;
        }
        let checks = [
            ("launch_factory", confirm_pons_launch_factory(http, &lp.launch_factory, &v4.pool_manager, &v4.permit2).await?),
            ("launch_router", confirm_pons_launch_router(http, &lp.launch_router, &lp.launch_factory).await?),
            ("graduation_hook", confirm_pons_meme_hook(http, &lp.graduation_hook, &lp.launch_factory).await?),
            ("uniswap_v4.pool_manager", confirm_uniswap_v4_pool_manager(http, &v4.pool_manager, &lp.launch_factory).await?),
            ("uniswap_v4.state_view", confirm_uniswap_v4_state_view(http, &v4.state_view, &v4.pool_manager).await?),
        ];
        for (name, r) in checks {
            println!("\n[{name}] {} → {}", r.address, r.verdict());
            for n in &r.notes {
                println!("    - {n}");
            }
            all_ok &= r.role_confirmed;
        }
    }
    if !all_ok {
        anyhow::bail!("al menos una dirección NO confirmó su rol on-chain — no usar para operar");
    }
    println!("\nTODAS las direcciones verificadas: bytecode + rol confirmados.");
    Ok(())
}
