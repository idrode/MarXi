//! Motor de trading. Dos sub-motores porque en Robinhood Chain un memecoin
//! recién lanzado en un launchpad tipo Pons NO se tradea igual antes y
//! después de graduar:
//!
//! - Fase bonding curve (pre-graduación): no hay pool, se compra/vende
//!   contra el contrato de curva propio del token. Ver `curve_engine`.
//! - Fase graduada: pool normal de Uniswap V4, con un hook que identifica
//!   de qué launchpad viene. Ver `v4_engine`.
//!
//! Ambos motores comparten el mismo checklist de seguridad antes de firmar
//! y enviar nada — ver `safety`. Ese checklist no es opcional por diseño:
//! cualquier función de "enviar swap" pasa por `safety::preflight` primero.

pub mod curve_engine;
pub mod v4_engine;
pub mod safety;
pub mod position;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeDirection {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct SwapRequest {
    pub token_address: String,
    pub direction: TradeDirection,
    pub amount_in: u128, // en unidades base del token de entrada (wei-equivalente)
    pub slippage_bps: u32,
    pub deadline_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct SwapResult {
    pub tx_hash: String,
    pub amount_out_estimated: u128,
    pub amount_out_actual: Option<u128>, // se rellena tras confirmación on-chain
}
