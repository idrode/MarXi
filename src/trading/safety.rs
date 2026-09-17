//! Checklist de seguridad que TODA transacción de swap debe pasar antes de
//! firmarse y enviarse. Diseñado para que sea imposible saltárselo por
//! accidente: `preflight` devuelve un `Result` y el llamador está obligado a
//! propagarlo, no hay un "modo rápido" que lo omita silenciosamente.
//!
//! Piezas cubiertas (ver conversación de diseño / CLAUDE.md del proyecto):
//! 1. Simulación previa (detectar revert o output muy distinto del esperado)
//! 2. Slippage máximo — nunca "sin límite"
//! 3. Approve acotado — nunca infinito por defecto
//! 4. Deadline en la propia transacción
//! 5. Honeypot check (solo aplica en fase graduada; en fase curve el riesgo
//!    equivalente es otro — ver `honeypot_or_curve_risk`)
//! 6. Confirmación explícita del usuario — esto vive en la capa de UI, pero
//!    `preflight` es lo que le da a la UI los datos para mostrar esa
//!    confirmación con información real, no aproximada.

use crate::trading::{SwapRequest, TradeDirection};

#[derive(Debug)]
pub struct PreflightReport {
    pub would_revert: bool,
    pub estimated_amount_out: u128,
    pub estimated_gas: u64,
    pub honeypot_risk: RiskAssessment,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum RiskAssessment {
    /// No se pudo evaluar (ej. provider de honeypot check no soporta esta
    /// chain todavía, o el token está en fase curve donde el check no aplica
    /// igual). Nunca se debe tratar como "seguro" — la UI debe dejarlo claro.
    NotChecked { reason: String },
    Low,
    Medium { details: Vec<String> },
    High { details: Vec<String> },
}

impl PreflightReport {
    /// Si esto devuelve false, la UI NO debería permitir continuar sin que
    /// el usuario reconozca explícitamente el riesgo (para High) o sin más
    /// información (para NotChecked).
    pub fn safe_to_proceed_without_extra_confirmation(&self) -> bool {
        !self.would_revert && matches!(self.honeypot_risk, RiskAssessment::Low)
    }
}

/// Punto único de entrada del checklist. Debe llamarse antes de construir
/// cualquier tx real, tanto en curve_engine como en v4_engine.
///
/// TODO(implementación real):
/// - Simulación: usar eth_call/eth_estimateGas contra el router/curva
///   correspondiente (recordar: Alchemy en esta chain NO soporta
///   debug_traceCall — no depender de trace para esto)
/// - Honeypot check: llamar al provider configurado (GoPlus u otro) SOLO si
///   el token ya graduó; en fase curve, evaluar en su lugar señales propias
///   del launchpad (ver honeypot_or_curve_risk)
/// - Deadline: calcular timestamp = now + cfg.trading.tx_deadline_seconds
/// - Approve: calcular el monto exacto necesario, nunca u256::MAX por defecto
pub async fn preflight(_req: &SwapRequest) -> anyhow::Result<PreflightReport> {
    todo!("implementar simulación + honeypot check + construcción de warnings")
}

/// En fase bonding-curve no aplica un honeypot-check tradicional (no hay
/// pool ni liquidez que "bloquear" todavía). El riesgo equivalente es otro:
/// ¿el creador tiene permisos anómalos sobre la curva?, ¿el token tiene
/// mecanismos no estándar (fees de transferencia, blacklist, pausable)?
/// Placeholder explícito para no fingir que un check de pool-graduado cubre
/// este caso.
pub async fn honeypot_or_curve_risk(
    _token_address: &str,
    _direction: TradeDirection,
) -> anyhow::Result<RiskAssessment> {
    todo!("implementar heurística de riesgo específica para fase bonding-curve")
}
