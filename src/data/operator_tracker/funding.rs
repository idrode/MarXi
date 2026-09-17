//! Financiación de una wallet de operador: de dónde le entra el dinero antes
//! de lanzar. Es la señal 1 del diseño ("posible lanzamiento inminente").
//!
//! Dos caminos, porque la chain los trata distinto:
//!
//! - **ERC-20**: emite `Transfer`, así que basta un `eth_getLogs` filtrado
//!   por `topics[2] == wallet` **sin `address`** (a lo largo de toda la
//!   chain, cualquier token). Barato e histórico completo.
//! - **ETH nativo**: no emite ningún log. El diseño aprobado el 2026-09-17
//!   lo daba por imposible por RPC puro y lo dejaba como limitación
//!   pendiente de Blockscout Pro. **Medido el 2026-09-17: el RPC de Alchemy
//!   de esta chain SÍ sirve estado histórico** (`eth_getBalance` a bloques de
//!   hace 50M responde), así que se detecta por **bisección sobre el saldo**:
//!   si el saldo subió entre dos bloques, hubo una entrada en medio; se parte
//!   el rango hasta el bloque exacto y ahí se lee el bloque completo para
//!   sacar remitente e importe. Cuesta ~log2(rango) llamadas por entrada, no
//!   un bloque por bloque.
//!
//! Límite conocido de la bisección: ve **saldo neto**. Si en el mismo tramo
//! entra y sale ETH, solo detecta el neto, y una entrada compensada por una
//! salida mayor se pierde. Por eso el rango se trocea por lanzamientos (hay
//! pocos bloques entre uno y otro) y por eso el conteo nativo es un **suelo**,
//! no un censo exacto. Se dice explícitamente al reportar.

use super::profile::{Funding, FundingAsset, OperatorProfile};
use crate::chain::ChainProvider;
use crate::data::abi::{IERC20Meta, Transfer};
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;
use std::collections::HashMap;

/// Tope de llamadas de saldo por perfil. La bisección es logarítmica, pero un
/// operador con cientos de entradas podría sumar muchas: mejor parar y
/// decirlo que agotar el rate limit a mitad.
const MAX_BALANCE_CALLS: usize = 1_200;

/// Resultado de medir la financiación de una wallet.
pub struct FundingScan {
    pub fundings: Vec<Funding>,
    /// Llamadas `eth_getBalance` gastadas en la bisección.
    pub balance_calls: usize,
    /// True si se llegó al tope: el resultado está **incompleto** y se avisa.
    pub truncated: bool,
    /// Tramos en los que el saldo subió pero no se encontró ninguna tx
    /// directa `to == wallet` en el bloque (entrada por llamada interna de un
    /// contrato). Se cuentan aparte en vez de atribuirlas a un remitente
    /// inventado.
    pub native_internal: usize,
}

/// Financiaciones en ERC-20: cualquier token, cualquier remitente.
pub async fn backfill_erc20_fundings(
    provider: &ChainProvider,
    wallet: Address,
    from_block: u64,
    to_block: u64,
    chunk_blocks: u64,
) -> anyhow::Result<Vec<Funding>> {
    // Sin `.address(...)`: interesa cualquier token, no uno concreto.
    let filter = Filter::new()
        .event_signature(Transfer::SIGNATURE_HASH)
        .topic2(wallet.into_word());
    let logs = provider
        .get_logs_backfill(&filter, from_block, to_block, chunk_blocks)
        .await?;

    // Los decimales se leen una vez por token, no una por transferencia.
    let mut decimals_cache: HashMap<Address, u8> = HashMap::new();
    let mut out = Vec::with_capacity(logs.len());
    for log in &logs {
        let block = log
            .block_number
            .ok_or_else(|| anyhow::anyhow!("un Transfer llegó sin blockNumber"))?;
        let token = log.address();
        let ev = Transfer::decode_log(&log.inner)
            .map_err(|e| anyhow::anyhow!("log en {block} no decodifica como Transfer: {e}"))?;

        let decimals = match decimals_cache.get(&token) {
            Some(d) => *d,
            None => {
                // Un token sin `decimals()` legible no se descarta: se asume
                // 18 y se deja constancia en el log.
                let d = IERC20Meta::new(token, provider.http())
                    .decimals()
                    .call()
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(%token, "decimals() no responde, se asume 18: {e}");
                        18
                    });
                decimals_cache.insert(token, d);
                d
            }
        };

        out.push(Funding {
            asset: FundingAsset::Erc20 { token, decimals },
            from: Some(ev.from),
            amount_raw: ev.value,
            amount: scaled(ev.value, decimals),
            block,
            timestamp: 0, // lo rellena quien componga el perfil
            tx_hash: log.transaction_hash,
        });
    }
    Ok(out)
}

/// Financiaciones en ETH nativo por bisección de saldo dentro de `[from, to]`.
///
/// Requiere un RPC con estado histórico (Alchemy sí; el público oficial
/// responde `historical state ... is not available`).
pub async fn find_native_fundings(
    provider: &ChainProvider,
    wallet: Address,
    from_block: u64,
    to_block: u64,
) -> anyhow::Result<FundingScan> {
    let mut scan = FundingScan {
        fundings: Vec::new(),
        balance_calls: 0,
        truncated: false,
        native_internal: 0,
    };
    if from_block >= to_block {
        return Ok(scan);
    }

    let lo_bal = balance_at(provider, wallet, from_block, &mut scan).await?;
    let hi_bal = balance_at(provider, wallet, to_block, &mut scan).await?;
    bisect(provider, wallet, from_block, lo_bal, to_block, hi_bal, &mut scan).await?;
    scan.fundings.sort_by_key(|f| f.block);
    Ok(scan)
}

/// Divide el rango mientras el saldo del extremo derecho sea mayor que el del
/// izquierdo. Cuando el tramo se reduce a un solo bloque, ese bloque contiene
/// la entrada: se lee entero para identificar remitente e importe reales.
///
/// Se implementa con pila explícita y no con recursión `async`, que en Rust
/// necesitaría boxear en cada nivel.
async fn bisect(
    provider: &ChainProvider,
    wallet: Address,
    from: u64,
    from_bal: U256,
    to: u64,
    to_bal: U256,
    scan: &mut FundingScan,
) -> anyhow::Result<()> {
    let mut stack = vec![(from, from_bal, to, to_bal)];
    while let Some((lo, lo_bal, hi, hi_bal)) = stack.pop() {
        if hi_bal <= lo_bal {
            continue; // en este tramo no entró nada neto
        }
        if scan.balance_calls >= MAX_BALANCE_CALLS {
            scan.truncated = true;
            return Ok(());
        }
        if hi - lo == 1 {
            record_native(provider, wallet, hi, hi_bal - lo_bal, scan).await?;
            continue;
        }
        let mid = lo + (hi - lo) / 2;
        let mid_bal = balance_at(provider, wallet, mid, scan).await?;
        // Los dos lados se examinan: puede haber más de una entrada en el
        // rango, y quedarse con el primero que suba perdería el resto.
        stack.push((lo, lo_bal, mid, mid_bal));
        stack.push((mid, mid_bal, hi, hi_bal));
    }
    Ok(())
}

/// Lee el bloque `block` entero y busca la tx que mandó ETH a la wallet. Si no
/// hay ninguna directa, la entrada llegó por una llamada interna de un
/// contrato: se registra igual, con `from: None`, en vez de atribuirla mal.
async fn record_native(
    provider: &ChainProvider,
    wallet: Address,
    block: u64,
    delta: U256,
    scan: &mut FundingScan,
) -> anyhow::Result<()> {
    // El bloque se pide como JSON crudo a propósito: los bloques de esta
    // chain (Arbitrum Nitro) incluyen transacciones de sistema con tipos
    // propios (`0x6a`, ArbitrumInternalTx) que el `Block` de alloy —tipado
    // para la red Ethereum— rechaza entero con
    // "data did not match any variant of untagged enum BlockTransactions".
    // Medido el 2026-09-17: con `.full()` tipado, el escaneo aborta.
    let raw: serde_json::Value = provider
        .http()
        .raw_request(
            "eth_getBlockByNumber".into(),
            (format!("0x{block:x}"), true),
        )
        .await?;

    let (mut from, mut tx_hash, mut timestamp) = (None, None, 0u64);
    if let Some(ts) = raw.get("timestamp").and_then(|v| v.as_str()) {
        timestamp = u64::from_str_radix(ts.trim_start_matches("0x"), 16).unwrap_or(0);
    }
    if let Some(txs) = raw.get("transactions").and_then(|v| v.as_array()) {
        for tx in txs {
            let to = tx.get("to").and_then(|v| v.as_str()).and_then(|s| s.parse::<Address>().ok());
            let value = tx
                .get("value")
                .and_then(|v| v.as_str())
                .and_then(|s| U256::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                .unwrap_or(U256::ZERO);
            if to == Some(wallet) && !value.is_zero() {
                from = tx.get("from").and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
                tx_hash = tx.get("hash").and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
                break;
            }
        }
    }
    if from.is_none() {
        scan.native_internal += 1;
    }

    scan.fundings.push(Funding {
        asset: FundingAsset::Native,
        from,
        amount_raw: delta,
        amount: scaled(delta, 18),
        block,
        timestamp,
        tx_hash,
    });
    Ok(())
}

async fn balance_at(
    provider: &ChainProvider,
    wallet: Address,
    block: u64,
    scan: &mut FundingScan,
) -> anyhow::Result<U256> {
    scan.balance_calls += 1;
    let mut attempt = 0u32;
    loop {
        match provider.http().get_balance(wallet).block_id(block.into()).await {
            Ok(v) => return Ok(v),
            // Mismo motivo que en el backfill de timestamps: el 429 de
            // Alchemy es por unidades de cómputo por segundo y se pasa solo.
            Err(e) if attempt < 3 => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(200 * attempt as u64)).await;
                let _ = e;
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "eth_getBalance en el bloque {block} falló ({e}). Si dice \
                     'historical state is not available', ese RPC no es archive \
                     y la financiación nativa no se puede medir por saldo."
                ))
            }
        }
    }
}

/// Reparto ETH nativo vs ERC-20 sobre un conjunto de financiaciones. Es el
/// dato que condiciona el punto (c) del diseño.
pub fn native_vs_erc20(fundings: &[Funding]) -> (usize, usize) {
    let native = fundings.iter().filter(|f| f.asset.is_native()).count();
    (native, fundings.len() - native)
}

/// U256 crudo a f64 en unidades del token. Vía `to_string` a propósito: un
/// supply o un importe puede no caber en u128, y aquí solo se necesita el
/// orden de magnitud para leerlo.
fn scaled(raw: U256, decimals: u8) -> f64 {
    raw.to_string().parse::<f64>().unwrap_or(f64::NAN) / 10f64.powi(decimals as i32)
}

/// Rellena los timestamps de las financiaciones ERC-20 (las nativas ya lo
/// traen del bloque leído) reutilizando el mismo esquema de anclas del
/// backfill por-token.
pub async fn fill_timestamps(provider: &ChainProvider, fundings: &mut [Funding]) -> anyhow::Result<()> {
    let mut blocks: Vec<u64> = fundings.iter().filter(|f| f.timestamp == 0).map(|f| f.block).collect();
    if blocks.is_empty() {
        return Ok(());
    }
    blocks.sort_unstable();
    blocks.dedup();
    let ts = crate::data::backfill::resolve_block_timestamps(provider, &blocks).await?;
    for f in fundings.iter_mut().filter(|f| f.timestamp == 0) {
        f.timestamp = ts.get(&f.block).copied().unwrap_or(0);
    }
    Ok(())
}

/// Financiaciones (de cualquier activo) que preceden a un lanzamiento dentro
/// de `window_secs`. Es el conjunto con el que después se calculará el
/// baseline; por ahora solo se cuenta y se mide.
pub fn fundings_before_launch<'a>(
    profile: &OperatorProfile,
    fundings: &'a [Funding],
    window_secs: u64,
) -> Vec<&'a Funding> {
    fundings
        .iter()
        .filter(|f| {
            profile.launches.iter().any(|l| {
                l.timestamp >= f.timestamp && l.timestamp - f.timestamp <= window_secs
            })
        })
        .collect()
}
