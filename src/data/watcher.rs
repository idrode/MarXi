//! Escucha eventos on-chain relevantes vía WebSocket (eth_subscribe) y
//! empuja `AppEvent`s al canal de la app.
//!
//! Tres tipos de suscripción, con alcance distinto:
//!
//! 1. Sniper (todos los launchpads habilitados): TokenLaunched / PoolGraduated
//!    (o el nombre de evento equivalente por launchpad — verificar ABI real
//!    de cada uno, no asumir que todos usan los mismos nombres que Pons).
//!    Esto es "factory-wide" dentro de cada launchpad, no de toda la chain.
//!
//! 2. Token activo en pantalla: swaps/CurveBuy/CurveSell del token que el
//!    usuario tiene seleccionado, para alimentar `candles` en tiempo real.
//!
//! 3. Holders/whale-tracking (fase 2, no MVP): Transfer del token activo —
//!    deliberadamente no se arranca por defecto porque es mucho más pesado
//!    que seguir solo swaps (todo transfer, no solo los de trading).

use crate::app::AppEvent;
use tokio::sync::mpsc::Sender;

pub struct Watcher {
    event_tx: Sender<AppEvent>,
}

impl Watcher {
    pub fn new(event_tx: Sender<AppEvent>) -> Self {
        Self { event_tx }
    }

    /// Arranca la escucha de lanzamientos/graduaciones de un launchpad.
    /// Se spawnea una tarea tokio independiente por launchpad habilitado —
    /// nunca bloquea el loop de la TUI.
    pub async fn spawn_launchpad_watcher(&self, _launchpad_name: String, _factory_address: String) {
        todo!(
            "eth_subscribe a logs del factory, decodificar TokenLaunched/ \
             PoolGraduated con el ABI real de Pons (pendiente de obtener y \
             fijar en el proyecto, no asumir de memoria), mandar AppEvent"
        )
    }

    /// Arranca la escucha de actividad (swaps o curve buys/sells) del token
    /// actualmente seleccionado en la UI. Se para y se vuelve a arrancar
    /// cada vez que el usuario cambia de token en pantalla.
    pub async fn spawn_token_activity_watcher(&self, _token_address: String, _phase: crate::app::state::TokenPhase) {
        todo!("suscripción condicionada a la fase: CurveBuy/CurveSell si aún no gradúa, Swap si ya gradúa")
    }
}
