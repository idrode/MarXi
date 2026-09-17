//! Chequeo de honeypot/riesgo de token antes de ofrecer comprar, para
//! tokens ya graduados a un pool de Uniswap V4 (fase bonding-curve usa un
//! chequeo distinto — ver `trading::safety::honeypot_or_curve_risk`).
//!
//! PENDIENTE DE VERIFICAR antes de depender de esto: si GoPlus Security
//! (o el proveedor que se acabe eligiendo) soporta Robinhood Chain
//! (chain_id 4663) en su API pública, no solo en alguna interfaz web de
//! terceros. No asumido todavía — comprobar con una llamada de prueba real
//! antes de construir el resto de este módulo sobre esa base.

#[derive(Debug)]
pub struct HoneypotCheckResult {
    pub is_honeypot: Option<bool>, // None si el proveedor no pudo evaluar
    pub buy_tax_bps: Option<u32>,
    pub sell_tax_bps: Option<u32>,
    pub raw_provider_notes: Vec<String>,
}

pub async fn check_via_goplus(_token_address: &str, _chain_id: u64) -> anyhow::Result<HoneypotCheckResult> {
    todo!(
        "llamar a la API de GoPlus Security con chain_id=4663 — VERIFICAR \
         PRIMERO que esta chain esté en su lista de chains soportadas vía \
         API antes de construir la lógica de parseo de la respuesta"
    )
}
