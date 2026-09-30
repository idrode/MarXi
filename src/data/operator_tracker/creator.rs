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
//!
//! **Vía intermediario: `tx.from` no siempre es el creador** (regla fijada el
//! 2026-09-30 antes de medir el lote de control, CLAUDE.md "Addendum al
//! pre-registro"). Hay una plataforma de lanzamiento programado en la que una
//! wallet configura una "campaña" (clon ERC-1167) y un ejecutor distinto la
//! dispara después; ahí el creador es el configurador. Ver [`Attribution`].

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

/// Evento que emite la campaña de la plataforma de lanzamiento programado en
/// la tx del lanzamiento (`topics[1..3]` = lanzador, token, curva). Medido el
/// 2026-09-30: 10 veces en los 48.989 lanzamientos del 22-27 sep.
const CAMPAIGN_LAUNCH_TOPIC: B256 =
    alloy::primitives::b256!("7e03abd06e00d4f7a39b42fcb7404becd5982043a2706a32b81cd64f363a5fcf");
/// Hasta dónde se busca hacia atrás la configuración de una campaña (el máximo
/// medido fue 32,5 h ≈ 1,2 M bloques; el RPC público admite 10 M con `address`).
const CAMPAIGN_LOOKBACK_BLOCKS: u64 = 9_000_000;

/// Cómo se decidió quién es el creador.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// El `deployer` de `TokenLaunched` es una wallet.
    Deployer,
    /// Quien firmó la tx del lanzamiento.
    TxFrom,
    /// La tx la disparó un ejecutor sobre una campaña; el creador es quien la
    /// configuró (firmante del primer log de la campaña). Dudosa.
    Campaign { campaign: Address, executor: Address },
    /// `tx.to` es un clon ERC-1167 distinto del lanzador y del firmante, sin
    /// evento de campaña: mismo patrón de ejecutor, plataforma no reconocida.
    /// Se queda `tx.from`, marcado como dudoso.
    TxFromViaClone { clone: Address },
}

impl Attribution {
    /// Etiqueta corta, estable, para CSV.
    pub fn code(self) -> &'static str {
        match self {
            Attribution::Deployer => "deployer",
            Attribution::TxFrom => "tx_from",
            Attribution::Campaign { .. } => "campana",
            Attribution::TxFromViaClone { .. } => "tx_from_via_clon",
        }
    }

    pub fn doubtful(self) -> bool {
        matches!(self, Attribution::Campaign { .. } | Attribution::TxFromViaClone { .. })
    }

    /// El firmante de la tx del lanzamiento cuando no es el creador.
    pub fn executor(self) -> Option<Address> {
        match self {
            Attribution::Campaign { executor, .. } => Some(executor),
            _ => None,
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
    pub attribution: Attribution,
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
        let (creator, creator_kind, via, attribution) = if deployer_kind.is_wallet() {
            (ev.deployer, deployer_kind, LaunchVia::PonsV2Direct, Attribution::Deployer)
        } else {
            // Contrato (o sin comprobar): la wallet humana es quien firmó,
            // salvo que la haya disparado un ejecutor.
            let (from, to) = tx_from_to(provider, tx).await?;
            let attribution =
                intermediary_attribution(provider, tx, from, to, ev.deployer, block, chunk_blocks).await?;
            let creator = match attribution {
                Attribution::Campaign { campaign, .. } => {
                    campaign_configurator(provider, campaign, block, chunk_blocks).await?
                }
                _ => from,
            };
            let kind = classify_deployer(provider, creator).await;
            (creator, kind, LaunchVia::PonsV2Intermediary { contract: ev.deployer }, attribution)
        };
        return Ok(TokenCreator {
            token,
            creator,
            creator_kind,
            via,
            attribution,
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
        attribution: Attribution::TxFrom,
        launch_block: block,
        launch_tx: tx,
        pair_token: None,
        curve: None,
        graduation_threshold: None,
    })
}

/// Regla de atribución de la vía intermediario (ver [`Attribution`]).
async fn intermediary_attribution(
    provider: &ChainProvider,
    tx: B256,
    from: Address,
    to: Option<Address>,
    deployer: Address,
    block: u64,
    chunk_blocks: u64,
) -> anyhow::Result<Attribution> {
    let Some(to) = to.filter(|&t| t != deployer && t != from) else {
        return Ok(Attribution::TxFrom);
    };
    let campaign = Filter::new().address(to).event_signature(CAMPAIGN_LAUNCH_TOPIC);
    let logs = provider.get_logs_backfill(&campaign, block, block, chunk_blocks).await?;
    if logs.iter().any(|l| l.transaction_hash == Some(tx)) {
        return Ok(Attribution::Campaign { campaign: to, executor: from });
    }
    let code = provider
        .http()
        .get_code_at(to)
        .await
        .map_err(|e| anyhow::anyhow!("eth_getCode({to}) falló: {e}"))?;
    Ok(if is_erc1167_clone(&code) {
        Attribution::TxFromViaClone { clone: to }
    } else {
        Attribution::TxFrom
    })
}

/// Runtime de un clon mínimo ERC-1167: 45 bytes con este prefijo.
fn is_erc1167_clone(code: &[u8]) -> bool {
    code.len() == 45 && code.starts_with(&[0x36, 0x3d, 0x3d, 0x37, 0x3d, 0x3d, 0x3d, 0x36, 0x3d, 0x73])
}

/// Quien configuró una campaña: el firmante de la tx de su primer log.
async fn campaign_configurator(
    provider: &ChainProvider,
    campaign: Address,
    launch_block: u64,
    chunk_blocks: u64,
) -> anyhow::Result<Address> {
    let from_block = launch_block.saturating_sub(CAMPAIGN_LOOKBACK_BLOCKS);
    let logs = provider
        .get_logs_backfill(&Filter::new().address(campaign), from_block, launch_block, chunk_blocks)
        .await?;
    let tx = logs.iter().find_map(|l| l.transaction_hash).ok_or_else(|| {
        anyhow::anyhow!("la campaña {campaign} no tiene logs entre {from_block} y {launch_block}: no se puede atribuir creador")
    })?;
    Ok(tx_from_to(provider, tx).await?.0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconoce_un_clon_erc1167() {
        let clone = alloy::primitives::hex::decode(
            "363d3d373d3d3d363d7342b0b14c6e6bcaa2e9b29f87aa3de19290a5c5725af43d82803e903d91602b57fd5bf3",
        )
        .unwrap();
        assert!(is_erc1167_clone(&clone));
        assert!(!is_erc1167_clone(&clone[..44]));
        assert!(!is_erc1167_clone(&[0u8; 45]));
    }

    #[test]
    fn solo_campana_y_clon_son_dudosas() {
        let a = Address::ZERO;
        assert!(!Attribution::Deployer.doubtful());
        assert!(!Attribution::TxFrom.doubtful());
        assert!(Attribution::Campaign { campaign: a, executor: a }.doubtful());
        assert!(Attribution::TxFromViaClone { clone: a }.doubtful());
    }
}
