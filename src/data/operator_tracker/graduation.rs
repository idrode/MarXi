//! Quién compró la curva de un token de Pons V2 hasta su graduación.
//!
//! Para separar dos éxitos que no se parecen en nada (pedido del usuario el
//! 2026-09-28): un token que gradúa porque **terceros** compran la curva, y uno
//! que gradúa porque **el propio creador** se la compra entera. Medido en el
//! lote de éxito: `0xd6c065…` graduó con **una sola compra, 100 % del creador,
//! en la misma tx del lanzamiento**; `0x314ad0…` también graduó en segundos,
//! pero con 54 compras y el creador en el 0,4 % — rápido no es autocompra, y
//! por eso el criterio mira compradores, no tiempo.
//!
//! Límite: se atribuye al creador una compra con `buyer` o `recipient` igual a
//! su wallet. Si compra a otras wallets suyas o a través de un contrato que se
//! queda los tokens, no se ve: el porcentaje es un **suelo**.

use crate::chain::ChainProvider;
use crate::data::abi::{CurveBuy, PoolGraduated};
use alloy::primitives::{Address, U256};
use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;
use std::collections::HashSet;

/// Umbral de `auto_graduado`: el creador compró al menos esta fracción del
/// quote que entró en la curva antes de graduar. Valores medidos en los 5
/// runners de Pons del lote de éxito: 0 %, 0,4 %, 0,8 %, 17,6 % y 100 % — sin
/// nada entre 18 % y 100 %; 50 % es "la mayoría", no un ajuste fino.
pub const AUTO_GRADUATION_MIN_CREATOR_SHARE: f64 = 0.5;

#[derive(Debug, Clone)]
pub struct GraduationBuyers {
    pub graduation_block: u64,
    /// `CurveBuy` en `[lanzamiento, graduación]`.
    pub buys: usize,
    pub distinct_recipients: usize,
    pub total_quote_in: U256,
    pub creator_quote_in: U256,
}

impl GraduationBuyers {
    /// Fracción del quote comprado por el creador (0..=1). `None` sin compras.
    pub fn creator_share(&self) -> Option<f64> {
        if self.total_quote_in.is_zero() {
            return None;
        }
        let f = |v: U256| v.to_string().parse::<f64>().unwrap_or(f64::NAN);
        Some(f(self.creator_quote_in) / f(self.total_quote_in))
    }

    pub fn is_self_graduated(&self) -> bool {
        self.creator_share().is_some_and(|s| s >= AUTO_GRADUATION_MIN_CREATOR_SHARE)
    }
}

/// `None` si el token no ha graduado (todavía). Solo Pons V2: en otro
/// launchpad no hay curva ni graduación.
pub async fn graduation_buyers(
    provider: &ChainProvider,
    factory: Address,
    token: Address,
    curve: Address,
    creator: Address,
    launch_block: u64,
    chunk_blocks: u64,
) -> anyhow::Result<Option<GraduationBuyers>> {
    use alloy::providers::Provider;
    let latest = provider.http().get_block_number().await?;
    let graduated = Filter::new()
        .address(factory)
        .event_signature(PoolGraduated::SIGNATURE_HASH)
        .topic1(token.into_word());
    let logs = provider.get_logs_backfill(&graduated, launch_block, latest, chunk_blocks).await?;
    let Some(graduation_block) = logs.first().and_then(|l| l.block_number) else {
        return Ok(None);
    };

    let buys_filter = Filter::new().address(curve).event_signature(CurveBuy::SIGNATURE_HASH);
    let buys = provider
        .get_logs_backfill(&buys_filter, launch_block, graduation_block, chunk_blocks)
        .await?;
    let mut out = GraduationBuyers {
        graduation_block,
        buys: buys.len(),
        distinct_recipients: 0,
        total_quote_in: U256::ZERO,
        creator_quote_in: U256::ZERO,
    };
    let mut recipients = HashSet::new();
    for log in &buys {
        let ev = CurveBuy::decode_log(&log.inner)
            .map_err(|e| anyhow::anyhow!("log en {:?} no decodifica como CurveBuy: {e}", log.block_number))?
            .data;
        recipients.insert(ev.recipient);
        out.total_quote_in += ev.quoteIn;
        if ev.buyer == creator || ev.recipient == creator {
            out.creator_quote_in += ev.quoteIn;
        }
    }
    out.distinct_recipients = recipients.len();
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(total: u64, creator: u64) -> GraduationBuyers {
        GraduationBuyers {
            graduation_block: 1,
            buys: 1,
            distinct_recipients: 1,
            total_quote_in: U256::from(total),
            creator_quote_in: U256::from(creator),
        }
    }

    #[test]
    fn auto_graduado_mira_la_parte_del_creador_no_el_tiempo() {
        // Valores medidos en el lote de éxito (2026-09-28).
        assert!(g(1000, 1000).is_self_graduated()); // 0xd6c065…: 100 %
        assert!(!g(1000, 176).is_self_graduated()); // 0xfad17d…: 17,6 %
        assert!(!g(1000, 4).is_self_graduated()); // 0x314ad0…: 0,4 %, aunque graduó en 4 s
        assert!(g(1000, 500).is_self_graduated()); // el umbral es inclusivo
        assert_eq!(g(0, 0).creator_share(), None); // sin compras no hay porcentaje
    }
}
