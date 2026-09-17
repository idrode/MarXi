//! Histórico bajo demanda de un token: trae sus propios eventos con
//! `eth_getLogs`, los convierte en puntos de precio, los agrega en velas y
//! reconstruye el número de holders.
//!
//! Va por el **provider de logs** (RPC público), no por el de calls: Alchemy
//! Free corta `eth_getLogs` en 10 bloques. Todo el troceado y los reintentos
//! los pone `ChainProvider::get_logs_backfill`.
//!
//! Dos detalles medidos el 2026-09-14 que condicionan el diseño:
//!
//! - Los logs del RPC público traen `blockTimestamp` a cero, así que los
//!   timestamps hay que pedirlos aparte. Pedir uno por bloque con trade no
//!   sirve: en JACKET son 1005 peticiones y **Alchemy Free devuelve 429 por
//!   exceso de unidades de cómputo por segundo** en la mayoría, lo que hacía
//!   perder dos tercios de los trades en silencio. La solución es muestrear
//!   anclas e interpolar entre ellas (`resolve_block_timestamps`).
//!   La interpolación es segura **por tramos, no global**: el ritmo de bloque
//!   de esta chain cambia a lo largo de su historia (medido ~0,100 s/bloque
//!   en 2026-05 y ~0,150 s/bloque en 2026-09), pero dentro de un tramo corto
//!   es constante, así que el error queda en segundos.
//! - Buscar el bloque de lanzamiento o de graduación se hace **hacia atrás**
//!   desde el último bloque, parando en el primer acierto: los tokens que se
//!   pegan en el buscador suelen ser recientes, y así el caso normal cuesta
//!   una sola petición en vez de recorrer toda la historia de la factory.

use crate::app::state::{TokenPhase, TokenView};
use crate::chain::ChainProvider;
use crate::data::abi::{CurveBuy, CurveSell, PoolGraduated, Swap, TokenLaunched, Transfer};
use crate::data::candles::{Candle, CandleAggregator};
use crate::data::token_lookup::{decimal_scale, price_from_sqrt_price_x96, u256_to_f64};
use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;
use std::collections::{HashMap, HashSet};

/// Primer bloque de la factory V2 activa. Nada anterior le pertenece, así que
/// ninguna búsqueda de eventos de Pons V2 baja de aquí. Vive en `data::abi`
/// para que el tracker de operadores y el backfill no tengan dos copias del
/// mismo número; se re-exporta aquí por compatibilidad.
pub use crate::data::abi::FACTORY_START_BLOCK;

/// Cuántos timestamps de bloque se piden a la vez. Alchemy Free limita las
/// unidades de cómputo por segundo, así que se mantiene bajo a propósito.
const TIMESTAMP_CONCURRENCY: usize = 4;
/// Reintentos por bloque ante un 429 de rate limit.
const TIMESTAMP_RETRIES: u32 = 4;
/// Cuántos bloques se consultan de verdad como anclas de interpolación. Con
/// esto un token cualquiera cuesta decenas de peticiones en vez de miles.
const MAX_TIMESTAMP_ANCHORS: usize = 48;

/// Un trade individual ya normalizado, sea de curva o de pool V4.
#[derive(Debug, Clone)]
pub struct TradePoint {
    pub block: u64,
    pub timestamp: u64,
    /// Precio de 1 token en unidades de pairToken.
    pub price: f64,
    /// Volumen del trade en unidades de pairToken.
    pub volume_pair: f64,
}

#[derive(Debug, Default)]
pub struct TokenHistory {
    pub trades: Vec<TradePoint>,
    pub candles: Vec<Candle>,
    pub holders: Option<usize>,
    pub launch_block: Option<u64>,
    pub graduation_block: Option<u64>,
    /// Cuántas peticiones `eth_getLogs` costó, para poder razonar sobre el
    /// coste real contra el rate limit del RPC público.
    pub transfer_events: usize,
    /// Bloques con trades cuyo timestamp no se pudo resolver ni interpolar.
    /// Debe ser cero: si no lo es, hay trades que no llegaron a las velas y
    /// el gráfico está incompleto.
    pub blocks_without_timestamp: usize,
}

impl TokenHistory {
    pub fn first_price(&self) -> Option<f64> {
        self.trades.first().map(|t| t.price)
    }
    pub fn last_price(&self) -> Option<f64> {
        self.trades.last().map(|t| t.price)
    }
    /// Variación entre el primer trade indexado y el último precio conocido.
    pub fn change_pct(&self, current_price: Option<f64>) -> Option<f64> {
        let first = self.first_price()?;
        let last = current_price.or_else(|| self.last_price())?;
        if first == 0.0 {
            return None;
        }
        Some((last / first - 1.0) * 100.0)
    }
}

/// Trae el histórico completo del token y lo agrega en velas.
///
/// `view` debe venir de `token_lookup::lookup`: de ahí salen la fase, la
/// curva, el PoolId y los decimales necesarios para valorar cada evento.
pub async fn backfill(
    provider: &ChainProvider,
    pool_manager: Address,
    factory: Address,
    token: Address,
    view: &TokenView,
    chunk_blocks: u64,
    candle_seconds: u64,
    count_holders: bool,
) -> anyhow::Result<TokenHistory> {
    let latest = provider.http().get_block_number().await?;
    let token_decimals = view.decimals.unwrap_or(18);
    let pair_decimals = view.pair_decimals.unwrap_or(18);

    let mut history = TokenHistory::default();

    // Bloque del TokenLaunched: delimita por abajo cualquier búsqueda de
    // eventos de este token, incluidos los Transfer.
    history.launch_block = find_event_block_backwards(
        provider,
        factory,
        TokenLaunched::SIGNATURE_HASH,
        token.into_word().into(),
        FACTORY_START_BLOCK,
        latest,
        chunk_blocks,
    )
    .await?;
    let launch_block = history.launch_block.unwrap_or(FACTORY_START_BLOCK);

    // Puntos de precio, según dónde se negocie el token.
    let raw_trades: Vec<(u64, f64, f64)> = match view.phase {
        TokenPhase::Graduated => {
            let pool_id: B256 = view
                .pool_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("token graduado sin pool_id resuelto"))?
                .parse()?;
            let grad = find_event_block_backwards(
                provider,
                factory,
                PoolGraduated::SIGNATURE_HASH,
                token.into_word().into(),
                launch_block,
                latest,
                chunk_blocks,
            )
            .await?;
            history.graduation_block = grad;

            // El memecoin es currency0 si su dirección es la menor. Mismo
            // criterio que usó token_lookup para derivar el PoolId.
            let pair: Address = view
                .pair_address
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("falta pair_address en el TokenView"))?
                .parse()?;
            let memecoin_is_currency0 = token < pair;

            let filter = Filter::new().address(pool_manager).event_signature(Swap::SIGNATURE_HASH).topic1(pool_id);
            let logs = provider
                .get_logs_backfill(&filter, grad.unwrap_or(launch_block), latest, chunk_blocks)
                .await?;

            let mut out = Vec::with_capacity(logs.len());
            for log in &logs {
                let Some(block) = log.block_number else { continue };
                let ev = Swap::decode_log(&log.inner)?;
                // El precio de la vela sale de sqrtPriceX96 (estado posterior
                // al swap), no del cociente de importes: ese último es el
                // precio medio de ejecución y se desvía hasta un ~10 % en
                // swaps grandes. Ver CLAUDE.md, "Paso 0".
                let price = price_from_sqrt_price_x96(
                    U256::from(ev.sqrtPriceX96),
                    memecoin_is_currency0,
                    token_decimals,
                    pair_decimals,
                );
                // El volumen se mide en el lado del par, que es el importe con
                // significado económico. amount0/amount1 son deltas del pool y
                // pueden ser negativos.
                let pair_amount = if memecoin_is_currency0 { ev.amount1 } else { ev.amount0 };
                let volume = i128_abs_scaled(pair_amount, pair_decimals);
                out.push((block, price, volume));
            }
            out
        }
        TokenPhase::BondingCurve | TokenPhase::Swept => {
            let curve: Address = view
                .curve_address
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("falta curve_address en el TokenView"))?
                .parse()?;
            let scale = decimal_scale(token_decimals, pair_decimals);

            let buys = provider
                .get_logs_backfill(
                    &Filter::new().address(curve).event_signature(CurveBuy::SIGNATURE_HASH),
                    launch_block,
                    latest,
                    chunk_blocks,
                )
                .await?;
            let sells = provider
                .get_logs_backfill(
                    &Filter::new().address(curve).event_signature(CurveSell::SIGNATURE_HASH),
                    launch_block,
                    latest,
                    chunk_blocks,
                )
                .await?;

            let mut out = Vec::with_capacity(buys.len() + sells.len());
            for log in &buys {
                let Some(block) = log.block_number else { continue };
                let ev = CurveBuy::decode_log(&log.inner)?;
                if ev.tokensOut.is_zero() {
                    continue;
                }
                // Precio NETO de comisiones: `quoteIn` incluye fee y tax, que
                // no entran en la reserva de la curva. Descontarlos es lo que
                // hace que el último trade cuadre con el precio spot de
                // getReserves; con el bruto la serie queda sesgada al alza.
                let net_in = ev.quoteIn.saturating_sub(ev.fee).saturating_sub(ev.tax);
                let price = (u256_to_f64(net_in) / u256_to_f64(ev.tokensOut)) * scale;
                out.push((block, price, scaled_u256(ev.quoteIn, pair_decimals)));
            }
            for log in &sells {
                let Some(block) = log.block_number else { continue };
                let ev = CurveSell::decode_log(&log.inner)?;
                if ev.tokensIn.is_zero() {
                    continue;
                }
                // En la venta el usuario recibe `quoteOut` ya neto, así que el
                // importe que salió de la reserva es quoteOut + fee + tax.
                let gross_out = ev.quoteOut + ev.fee + ev.tax;
                let price = (u256_to_f64(gross_out) / u256_to_f64(ev.tokensIn)) * scale;
                out.push((block, price, scaled_u256(ev.quoteOut, pair_decimals)));
            }
            // Compras y ventas vienen en dos consultas separadas: hay que
            // reordenarlas para que las velas salgan en orden temporal.
            out.sort_by_key(|(b, _, _)| *b);
            out
        }
        TokenPhase::Rescued | TokenPhase::Unknown => Vec::new(),
    };

    // Holders: reconstruir balances desde los Transfer. Medido en un token de
    // referencia: del orden de miles de eventos, el mismo coste que los swaps.
    if count_holders {
        let transfers = provider
            .get_logs_backfill(
                &Filter::new().address(token).event_signature(Transfer::SIGNATURE_HASH),
                launch_block,
                latest,
                chunk_blocks,
            )
            .await?;
        history.transfer_events = transfers.len();
        let mut balances: HashMap<Address, U256> = HashMap::new();
        for log in &transfers {
            let ev = Transfer::decode_log(&log.inner)?;
            if ev.from != Address::ZERO {
                let e = balances.entry(ev.from).or_default();
                *e = e.saturating_sub(ev.value);
            }
            if ev.to != Address::ZERO {
                let e = balances.entry(ev.to).or_default();
                *e = e.saturating_add(ev.value);
            }
        }
        // La dirección cero ya se excluye al construir el mapa (acuñaciones y
        // quemas). El resto con saldo positivo son los holders.
        history.holders = Some(balances.values().filter(|v| !v.is_zero()).count());
    }

    // Timestamps reales de los bloques que tienen trades.
    let mut blocks: Vec<u64> = raw_trades
        .iter()
        .map(|(b, _, _)| *b)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    blocks.sort_unstable();
    let timestamps = resolve_block_timestamps(provider, &blocks).await?;
    tracing::debug!(
        raw_trades = raw_trades.len(),
        unique_blocks = blocks.len(),
        resolved = timestamps.len(),
        "etapas del backfill"
    );
    history.blocks_without_timestamp = blocks.iter().filter(|b| !timestamps.contains_key(b)).count();

    let mut agg = CandleAggregator::new(candle_seconds);
    for (block, price, volume) in raw_trades {
        let Some(&ts) = timestamps.get(&block) else {
            tracing::warn!(block, "trade descartado: sin timestamp");
            continue;
        };
        if !price.is_finite() || price <= 0.0 {
            tracing::warn!(block, price, "trade descartado por precio no válido");
            continue;
        }
        agg.ingest(ts, price, volume);
        history.trades.push(TradePoint { block, timestamp: ts, price, volume_pair: volume });
    }
    history.candles = agg.snapshot();
    Ok(history)
}

/// Busca el bloque del primer log que case con `topic0` + `topic1`, recorriendo
/// el rango **de atrás hacia adelante** y parando en el primer tramo con
/// resultados. Devuelve el bloque del log más antiguo dentro de ese tramo.
///
/// Para un token recién lanzado esto cuesta una petición. Para uno antiguo
/// cuesta tantas como tramos haya, igual que recorrerlo hacia adelante.
async fn find_event_block_backwards(
    provider: &ChainProvider,
    address: Address,
    topic0: B256,
    topic1: B256,
    from_block: u64,
    to_block: u64,
    chunk_blocks: u64,
) -> anyhow::Result<Option<u64>> {
    let base = Filter::new().address(address).event_signature(topic0).topic1(topic1);
    let mut high = to_block;
    loop {
        let low = high.saturating_sub(chunk_blocks - 1).max(from_block);
        let logs = provider.get_logs_backfill(&base, low, high, chunk_blocks).await?;
        if let Some(b) = logs.iter().filter_map(|l| l.block_number).min() {
            return Ok(Some(b));
        }
        if low <= from_block {
            return Ok(None);
        }
        high = low - 1;
    }
}

/// Timestamps para una lista de bloques **ya ordenada**, sin descartar
/// ninguno.
///
/// Consulta de verdad como mucho `MAX_TIMESTAMP_ANCHORS` bloques repartidos
/// sobre la lista, y deduce el resto por interpolación lineal entre las dos
/// anclas que lo rodean. Las anclas se eligen sobre el vector de bloques con
/// trades, no sobre el rango de bloques, así que se concentran donde hay
/// actividad y la interpolación siempre trabaja sobre tramos con datos.
///
/// Si la lista es corta se piden todos y el resultado es exacto. Ese es el
/// caso normal de un token recién lanzado.
pub(crate) async fn resolve_block_timestamps(
    provider: &ChainProvider,
    blocks: &[u64],
) -> anyhow::Result<HashMap<u64, u64>> {
    if blocks.is_empty() {
        return Ok(HashMap::new());
    }

    // Índices de las anclas: primero y último siempre, más un reparto
    // uniforme por el medio.
    let anchor_idx: Vec<usize> = if blocks.len() <= MAX_TIMESTAMP_ANCHORS {
        (0..blocks.len()).collect()
    } else {
        let last = blocks.len() - 1;
        let mut v: Vec<usize> = (0..MAX_TIMESTAMP_ANCHORS)
            .map(|i| i * last / (MAX_TIMESTAMP_ANCHORS - 1))
            .collect();
        v.dedup();
        v
    };
    let anchor_blocks: Vec<u64> = anchor_idx.iter().map(|&i| blocks[i]).collect();
    let anchors = fetch_block_timestamps(provider, &anchor_blocks).await?;

    // Anclas que de verdad se resolvieron, en orden.
    let known: Vec<(u64, u64)> = {
        let mut v: Vec<(u64, u64)> = anchors.into_iter().collect();
        v.sort_unstable_by_key(|(b, _)| *b);
        v
    };
    if known.is_empty() {
        anyhow::bail!(
            "no se pudo obtener el timestamp de ningún bloque: sin ellos no hay velas posibles"
        );
    }

    let mut out = HashMap::with_capacity(blocks.len());
    for &b in blocks {
        let ts = match known.binary_search_by_key(&b, |(kb, _)| *kb) {
            Ok(i) => known[i].1,
            Err(pos) => {
                if pos == 0 {
                    // Antes de la primera ancla: se extrapola con el ritmo
                    // del primer tramo conocido.
                    extrapolate(known.first(), known.get(1), b)
                } else if pos >= known.len() {
                    extrapolate(known.last(), known.get(known.len().wrapping_sub(2)), b)
                } else {
                    let (b0, t0) = known[pos - 1];
                    let (b1, t1) = known[pos];
                    interpolate(b0, t0, b1, t1, b)
                }
            }
        };
        out.insert(b, ts);
    }
    Ok(out)
}

fn interpolate(b0: u64, t0: u64, b1: u64, t1: u64, b: u64) -> u64 {
    if b1 == b0 {
        return t0;
    }
    let frac = (b - b0) as f64 / (b1 - b0) as f64;
    t0 + ((t1 as f64 - t0 as f64) * frac).round() as u64
}

/// Extrapola con el ritmo de bloque del tramo conocido más cercano. Solo
/// entra en juego si falta un ancla en un extremo.
fn extrapolate(near: Option<&(u64, u64)>, other: Option<&(u64, u64)>, b: u64) -> u64 {
    let Some(&(nb, nt)) = near else { return 0 };
    let Some(&(ob, ot)) = other else { return nt };
    if ob == nb {
        return nt;
    }
    let rate = (ot as f64 - nt as f64) / (ob as f64 - nb as f64);
    let delta = (b as f64 - nb as f64) * rate;
    (nt as f64 + delta).max(0.0).round() as u64
}

/// Pide el timestamp de cada bloque, con concurrencia acotada y reintentos
/// ante el 429 de rate limit de Alchemy. Los bloques que aun así fallen se
/// omiten del mapa: quien llama decide si interpolarlos.
async fn fetch_block_timestamps(
    provider: &ChainProvider,
    blocks: &[u64],
) -> anyhow::Result<HashMap<u64, u64>> {
    let mut out = HashMap::with_capacity(blocks.len());
    for chunk in blocks.chunks(TIMESTAMP_CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for &b in chunk {
            let p = provider.http().clone();
            set.spawn(async move {
                let mut attempt = 0;
                loop {
                    let r = p
                        .get_block_by_number(alloy::eips::BlockNumberOrTag::Number(b))
                        .await
                        .map(|blk| blk.map(|blk| blk.header.timestamp));
                    match r {
                        Ok(v) => return (b, Ok(v)),
                        Err(e) if attempt < TIMESTAMP_RETRIES => {
                            attempt += 1;
                            // El 429 de Alchemy es por unidades de cómputo por
                            // segundo: esperar un poco lo resuelve.
                            tokio::time::sleep(std::time::Duration::from_millis(
                                200 * attempt as u64,
                            ))
                            .await;
                            let _ = e;
                        }
                        Err(e) => return (b, Err(e)),
                    }
                }
            });
        }
        while let Some(joined) = set.join_next().await {
            let (block, ts) = joined?;
            match ts {
                Ok(Some(ts)) => {
                    out.insert(block, ts);
                }
                Ok(None) => tracing::warn!(block, "el bloque no existe al pedir su timestamp"),
                Err(e) => tracing::warn!(block, "timestamp del bloque no disponible: {e}"),
            }
        }
    }
    Ok(out)
}

fn scaled_u256(raw: U256, decimals: u8) -> f64 {
    u256_to_f64(raw) / 10f64.powi(decimals as i32)
}

fn i128_abs_scaled(v: i128, decimals: u8) -> f64 {
    (v.unsigned_abs() as f64) / 10f64.powi(decimals as i32)
}
