//! Creador real de un token, partiendo de la dirección del token.
//!
//! No basta con el `deployer` de `TokenLaunched`: medido el 2026-09-27 con
//! runners de Phanes (deuda nº17 de CLAUDE.md), un creador puede lanzar
//!
//! - **directo** en Pons V2 — el `deployer` es su wallet;
//! - por un **contrato intermediario** sobre Pons V2 — el `deployer` es el
//!   contrato (caso real `0x0e1651ae…`, 152 lanzamientos) y la wallet humana
//!   solo aparece como `tx.from`;
//! - en **otro launchpad** — no hay `TokenLaunched` de Pons en absoluto (caso
//!   real `0x1eef016f…`, directo a Uniswap V4).
//!
//! Método, igual para los tres casos:
//!
//! 1. **Bloque de creación del token por bisección de `eth_getCode`** con
//!    estado histórico (Alchemy es archive en esta chain; ~27 llamadas). Evita
//!    un `eth_getLogs` de rango completo por token, que es lo que agota el RPC
//!    público en un lote.
//! 2. En ese único bloque: el `TokenLaunched` de la factory con
//!    `topics[1] = token`. Si está, el `deployer` sale de ahí; si el deployer es
//!    una wallet es el creador, y si es un contrato el creador es `tx.from`.
//! 3. Si no hay `TokenLaunched`, el mint (`Transfer` desde `0x0` emitido por el
//!    token) da la tx de creación: creador = `tx.from`, y `tx.to` es la entrada
//!    del otro launchpad.
//!
//! Ningún caso ambiguo se resuelve en silencio: si no aparece la tx de
//! creación, es un error con el bloque, no un creador inventado.

use super::history::classify_deployer;
use super::profile::DeployerKind;
use crate::chain::ChainProvider;
use crate::data::abi::{decode_token_launched, Transfer, TokenLaunched};
use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;

/// Por dónde se lanzó el token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchVia {
    /// `TokenLaunched` de Pons V2 con una wallet como `deployer`.
    PonsV2Direct,
    /// `TokenLaunched` de Pons V2 con un contrato como `deployer`: el
    /// creador es `tx.from`. Punto ciego de la watchlist (deuda nº17).
    PonsV2Intermediary { contract: Address },
    /// Sin `TokenLaunched` de Pons: otro launchpad (o un deploy a mano).
    /// `entry` es el `tx.to` de la tx de creación.
    Other { entry: Option<Address> },
}

impl LaunchVia {
    /// Etiqueta corta, estable, para CSV.
    pub fn code(self) -> &'static str {
        match self {
            LaunchVia::PonsV2Direct => "pons_v2_directo",
            LaunchVia::PonsV2Intermediary { .. } => "pons_v2_intermediario",
            LaunchVia::Other { .. } => "otro_launchpad",
        }
    }

    /// El contrato por el que pasó el lanzamiento, si no fue directo.
    pub fn contract(self) -> Option<Address> {
        match self {
            LaunchVia::PonsV2Direct => None,
            LaunchVia::PonsV2Intermediary { contract } => Some(contract),
            LaunchVia::Other { entry } => entry,
        }
    }
}

/// El creador de un token y su lanzamiento.
#[derive(Debug, Clone)]
pub struct TokenCreator {
    pub token: Address,
    pub creator: Address,
    pub creator_kind: DeployerKind,
    pub via: LaunchVia,
    /// Bloque en el que el token pasó a tener bytecode (== bloque del
    /// lanzamiento: la factory o el launchpad lo despliegan en la misma tx).
    pub launch_block: u64,
    pub launch_tx: B256,
    /// Solo en Pons V2. En otro launchpad no se sabe sin su ABI.
    pub pair_token: Option<Address>,
    pub curve: Option<Address>,
    pub graduation_threshold: Option<U256>,
}

/// Resuelve el creador real de `token`. Ver el doc del módulo.
pub async fn resolve_token_creator(
    provider: &ChainProvider,
    factory: Address,
    token: Address,
    chunk_blocks: u64,
) -> anyhow::Result<TokenCreator> {
    let block = creation_block(provider, token).await?;

    // (2) ¿Lo lanzó la factory de Pons V2 en ese bloque?
    let launched = Filter::new()
        .address(factory)
        .event_signature(TokenLaunched::SIGNATURE_HASH)
        .topic1(token.into_word());
    let logs = provider.get_logs_backfill(&launched, block, block, chunk_blocks).await?;
    if let Some(log) = logs.first() {
        let ev = decode_token_launched(log)?.data;
        let tx = log
            .transaction_hash
            .ok_or_else(|| anyhow::anyhow!("el TokenLaunched de {token} llegó sin transactionHash"))?;
        let deployer_kind = classify_deployer(provider, ev.deployer).await;
        let (creator, creator_kind, via) = if deployer_kind.is_wallet() {
            (ev.deployer, deployer_kind, LaunchVia::PonsV2Direct)
        } else {
            // Contrato (o sin comprobar): la wallet humana es quien firmó.
            let (from, _) = tx_from_to(provider, tx).await?;
            let kind = classify_deployer(provider, from).await;
            (from, kind, LaunchVia::PonsV2Intermediary { contract: ev.deployer })
        };
        return Ok(TokenCreator {
            token,
            creator,
            creator_kind,
            via,
            launch_block: block,
            launch_tx: tx,
            pair_token: Some(ev.pairToken),
            curve: Some(ev.curve),
            graduation_threshold: Some(ev.graduationThreshold),
        });
    }

    // (3) Otro launchpad: la tx del mint.
    let mint = Filter::new()
        .address(token)
        .event_signature(Transfer::SIGNATURE_HASH)
        .topic1(B256::ZERO);
    let logs = provider.get_logs_backfill(&mint, block, block, chunk_blocks).await?;
    let tx = logs.iter().find_map(|l| l.transaction_hash).ok_or_else(|| {
        anyhow::anyhow!(
            "{token}: tiene bytecode desde el bloque {block} pero ni TokenLaunched de Pons V2 ni \
             mint en ese bloque; no se puede atribuir creador"
        )
    })?;
    let (from, to) = tx_from_to(provider, tx).await?;
    let creator_kind = classify_deployer(provider, from).await;
    Ok(TokenCreator {
        token,
        creator: from,
        creator_kind,
        via: LaunchVia::Other { entry: to },
        launch_block: block,
        launch_tx: tx,
        pair_token: None,
        curve: None,
        graduation_threshold: None,
    })
}

/// Primer bloque en el que `token` tiene bytecode, por bisección.
///
/// Invariante: sin código en `lo`, con código en `hi`. Requiere estado
/// histórico (Alchemy); un RPC sin él falla aquí con su propio error.
async fn creation_block(provider: &ChainProvider, token: Address) -> anyhow::Result<u64> {
    let latest = provider.http().get_block_number().await?;
    let has_code = |b: u64| async move {
        provider
            .http()
            .get_code_at(token)
            .block_id(b.into())
            .await
            .map(|c| !c.is_empty())
            .map_err(|e| anyhow::anyhow!("eth_getCode({token}) en el bloque {b} falló: {e}"))
    };
    anyhow::ensure!(
        has_code(latest).await?,
        "{token} no tiene bytecode en esta chain: no es un token (¿es una wallet, u otra chain?)"
    );
    let (mut lo, mut hi) = (0u64, latest);
    anyhow::ensure!(!has_code(lo).await?, "{token} tiene bytecode desde el bloque 0");
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if has_code(mid).await? {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Ok(hi)
}

/// `from` y `to` de una tx. JSON crudo porque esta chain (Arbitrum Nitro)
/// tiene tipos de tx propios que el `Transaction` de alloy rechaza enteros.
async fn tx_from_to(provider: &ChainProvider, tx: B256) -> anyhow::Result<(Address, Option<Address>)> {
    let raw: serde_json::Value = provider
        .http()
        .raw_request("eth_getTransactionByHash".into(), (tx.to_string(),))
        .await?;
    let from = raw
        .get("from")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<Address>().ok())
        .ok_or_else(|| anyhow::anyhow!("la tx {tx} no trae `from` legible"))?;
    let to = raw.get("to").and_then(|v| v.as_str()).and_then(|s| s.parse::<Address>().ok());
    Ok((from, to))
}
