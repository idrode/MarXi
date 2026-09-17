//! Backfill del historial de lanzamientos de un deployer.
//!
//! Es barato porque `deployer` es indexed (`topics[3]` de `TokenLaunched`,
//! ver `data::abi`): el nodo filtra y devuelve solo los lanzamientos de esa
//! wallet, no los del launchpad entero.
//!
//! Disciplina heredada del backfill por-token, y por las mismas razones:
//! - los `eth_getLogs` van por `ChainProvider::get_logs_backfill`, que trocea
//!   el rango y reintenta ante 429/timeout del RPC público;
//! - los timestamps se resuelven por anclas + interpolación, nunca uno por
//!   bloque (Alchemy Free tira 429 por unidades de cómputo por segundo);
//! - ningún log se descarta en silencio: si un log no decodifica, la función
//!   devuelve error.

use super::profile::{DeployerKind, OperatorProfile, PastLaunch};
use crate::chain::ChainProvider;
use crate::data::abi::{decode_token_launched, token_launched_by_deployer_filter, FACTORY_START_BLOCK};
use crate::data::backfill::resolve_block_timestamps;
use alloy::primitives::Address;
use alloy::providers::Provider;

/// ¿Es `address` una wallet o un contrato?
///
/// Se comprueba **antes** de perfilar o de meter nada en la watchlist: un
/// deployer con bytecode es un relay (caso real: Multicall3), y la premisa
/// del tracker no le aplica. No aborta — devuelve la clasificación para que
/// quien llame avise; hay casos legítimos en que interesa mirar un relay.
///
/// Un fallo del `eth_getCode` da `Unknown`, nunca `Wallet`: no confundir "no
/// evaluado" con "evaluado y es una wallet".
/// Con bytecode hay que mirar **qué** bytecode: una EOA con delegación
/// EIP-7702 devuelve exactamente `0xef0100 ‖ address` (23 bytes) y sigue
/// siendo una wallet. Tratarla como contrato la sacaría de la watchlist sin
/// motivo — caso real: `0xadf3672e…`, 23 lanzamientos.
pub async fn classify_deployer(provider: &ChainProvider, address: Address) -> DeployerKind {
    match crate::chain::verify::has_bytecode(provider.http(), &address.to_string()).await {
        Ok(true) => match provider.http().get_code_at(address).await {
            Ok(code) => match delegation_target(code.as_ref()) {
                Some(delegate) => DeployerKind::DelegatedWallet { delegate },
                None => DeployerKind::PossibleRelayContract,
            },
            Err(e) => {
                tracing::warn!(%address, "no se pudo leer el bytecode del deployer: {e}");
                DeployerKind::Unknown
            }
        },
        Ok(false) => DeployerKind::Wallet,
        Err(e) => {
            tracing::warn!(%address, "no se pudo comprobar si el deployer es un contrato: {e}");
            DeployerKind::Unknown
        }
    }
}

/// Extrae la dirección delegada de un bytecode EIP-7702
/// (`0xef0100 ‖ address`, 23 bytes exactos). `None` si no es ese patrón.
fn delegation_target(code: &[u8]) -> Option<Address> {
    if code.len() == 23 && code[0] == 0xef && code[1] == 0x01 && code[2] == 0x00 {
        Some(Address::from_slice(&code[3..23]))
    } else {
        None
    }
}

/// Trae todos los `TokenLaunched` cuyo deployer es `deployer`, desde el
/// arranque de la factory V2 hasta `latest`, y los devuelve como perfil.
pub async fn backfill_operator_history(
    provider: &ChainProvider,
    factory: Address,
    deployer: Address,
    chunk_blocks: u64,
) -> anyhow::Result<OperatorProfile> {
    let kind = classify_deployer(provider, deployer).await;
    let latest = provider.http().get_block_number().await?;
    let filter = token_launched_by_deployer_filter(factory, deployer);
    let logs = provider
        .get_logs_backfill(&filter, FACTORY_START_BLOCK, latest, chunk_blocks)
        .await?;

    // Decodificar primero: si la firma ya no casa con la chain, mejor fallar
    // aquí que después con una lista a medias.
    let mut decoded = Vec::with_capacity(logs.len());
    for log in &logs {
        let block = log
            .block_number
            .ok_or_else(|| anyhow::anyhow!("un TokenLaunched llegó sin blockNumber: no se puede fechar"))?;
        let tx_hash = log
            .transaction_hash
            .ok_or_else(|| anyhow::anyhow!("un TokenLaunched llegó sin transactionHash"))?;
        let ev = decode_token_launched(log)?;
        decoded.push((block, tx_hash, ev.data));
    }
    decoded.sort_unstable_by_key(|(b, _, _)| *b);

    let blocks: Vec<u64> = {
        let mut v: Vec<u64> = decoded.iter().map(|(b, _, _)| *b).collect();
        v.dedup();
        v
    };
    let timestamps = if blocks.is_empty() {
        Default::default()
    } else {
        resolve_block_timestamps(provider, &blocks).await?
    };

    let launches = decoded
        .into_iter()
        .map(|(block, tx_hash, ev)| PastLaunch {
            token: ev.token,
            curve: ev.curve,
            pair_token: ev.pairToken,
            graduation_threshold: ev.graduationThreshold,
            block,
            timestamp: timestamps.get(&block).copied().unwrap_or(0),
            tx_hash,
        })
        .collect();

    Ok(OperatorProfile {
        address: deployer,
        label: None,
        kind,
        launches,
        history_from_block: FACTORY_START_BLOCK,
        history_to_block: latest,
    })
}
