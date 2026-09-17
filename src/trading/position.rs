//! Gestión de posiciones abiertas: PnL en vivo, take-profit / stop-loss.
//!
//! Diseño intencional: este módulo NO firma ni envía transacciones
//! directamente. Cuando detecta que un TP/SL se cumple, emite un
//! `AppEvent`/intención de venta que pasa por el mismo `safety::preflight`
//! que cualquier otra operación — el TP/SL automático no es una vía rápida
//! que se salte el checklist de seguridad.

use crate::app::state::Position;

pub struct PositionManager {
    pub positions: Vec<Position>,
}

impl PositionManager {
    pub fn new() -> Self {
        Self { positions: Vec::new() }
    }

    pub fn open(&mut self, position: Position) {
        self.positions.push(position);
    }

    /// Comprueba TP/SL contra un precio actual. Devuelve las posiciones que
    /// deberían cerrarse — el llamador decide cómo encolarlas para venta
    /// real (pasando siempre por safety::preflight).
    pub fn positions_to_close(&self, current_prices: &std::collections::HashMap<String, f64>) -> Vec<&Position> {
        self.positions
            .iter()
            .filter(|p| {
                let Some(&price) = current_prices.get(&p.token_address) else {
                    return false;
                };
                let hit_tp = p.take_profit.map(|tp| price >= tp).unwrap_or(false);
                let hit_sl = p.stop_loss.map(|sl| price <= sl).unwrap_or(false);
                hit_tp || hit_sl
            })
            .collect()
    }
}

impl Default for PositionManager {
    fn default() -> Self {
        Self::new()
    }
}
