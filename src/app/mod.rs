//! Estado global de la TUI y el bus de eventos que conecta las tareas async
//! (chain, data, trading) con el loop de render.
//!
//! Patrón (igual que en hyperT/marxi): tokio::spawn para cada tarea de fondo
//! (listener de eventos on-chain, indexador, etc.) que nunca toca la UI
//! directamente — todas mandan `AppEvent` por un canal `mpsc` de vuelta al
//! loop principal, que es el único que muta el estado que se renderiza.
//! Nunca bloquear el render loop con trabajo de red o de disco.

use tokio::sync::mpsc;

pub mod state;

pub use state::AppState;

/// Eventos que las tareas de fondo mandan al loop principal.
/// Deliberadamente plano por ahora — se irá tipando más fino según se
/// implementen `data::watcher` y `trading::engine`.
#[derive(Debug)]
pub enum AppEvent {
    /// Nuevo bloque visto por el listener de chain (para heartbeat/latencia).
    NewBlock { number: u64 },
    /// Un token nuevo fue lanzado en un launchpad soportado (evento TokenLaunched
    /// o equivalente). Aún no gradúa, solo existe la bonding curve.
    TokenLaunched {
        launchpad: String,
        token_address: String,
    },
    /// Un token graduó de bonding curve a pool de Uniswap V4.
    TokenGraduated {
        launchpad: String,
        token_address: String,
        pool_id: String,
    },
    /// Nueva vela agregada disponible para el par que se está siguiendo en pantalla.
    CandleUpdate {
        token_address: String,
        // OHLCV real vendrá tipado desde `data::candles` — placeholder aquí.
    },

    // --- buscador por-token (Fase 1) ---
    /// La consulta de estado on-chain ha empezado.
    TokenLookupStarted { address: String },
    /// Estado on-chain resuelto. Llega antes que el histórico porque es
    /// mucho más rápido: la UI ya puede pintar precio y market cap mientras
    /// el backfill sigue trabajando.
    TokenStateResolved { view: Box<state::TokenView> },
    /// Histórico terminado: velas, holders y variación.
    TokenHistoryResolved {
        view: Box<state::TokenView>,
        candles: Vec<crate::data::candles::Candle>,
    },
    /// La búsqueda falló (dirección inválida, token que no es de Pons V2,
    /// RPC caído...). El texto va tal cual al panel.
    TokenLookupFailed { message: String },

    // --- input de terminal ---
    // Van por aquí a propósito: así `apply_event` sigue siendo la única
    // función que muta `AppState`, que es una invariante del proyecto.
    /// Carácter tecleado en el campo de búsqueda.
    SearchInputChar(char),
    /// Borrar el último carácter del campo de búsqueda.
    SearchInputBackspace,
    /// Vaciar el campo de búsqueda.
    SearchInputClear,
    /// Cambiar de pestaña.
    TabSelected(state::Tab),
    /// Salir de la aplicación.
    Quit,
    /// Latido del render loop. Solo alterna el cursor del campo de búsqueda;
    /// existe para que ni eso mute el estado fuera de `apply_event`.
    Tick,
    /// Resultado de una operación de trading (éxito/fallo), para reflejar en UI.
    TradeResult {
        tx_hash: Option<String>,
        success: bool,
        message: String,
    },
    /// Error no fatal de alguna tarea de fondo (RPC caído, reconectando, etc.)
    BackgroundError { source: String, message: String },
}

pub struct App {
    pub state: AppState,
    pub event_tx: mpsc::Sender<AppEvent>,
    pub event_rx: mpsc::Receiver<AppEvent>,
}

impl App {
    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        Self {
            state: AppState::default(),
            event_tx,
            event_rx,
        }
    }

    /// Aplica un AppEvent al estado. Es la ÚNICA función que debería mutar
    /// `self.state` a partir de eventos async — mantiene el render loop
    /// puramente síncrono y predecible.
    pub fn apply_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::NewBlock { number } => {
                self.state.last_block_seen = Some(number);
            }
            AppEvent::TokenLaunched { launchpad, token_address } => {
                self.state.recent_launches.push((launchpad, token_address));
            }
            AppEvent::TokenGraduated { token_address, pool_id, .. } => {
                self.state.recent_graduations.push((token_address, pool_id));
            }
            AppEvent::CandleUpdate { .. } => {
                // TODO: empujar a la serie de velas del token activo en pantalla
            }
            AppEvent::TradeResult { message, success, .. } => {
                self.state.last_trade_message = Some((success, message));
            }
            AppEvent::BackgroundError { source, message } => {
                tracing::warn!(%source, %message, "error no fatal en tarea de fondo");
                self.state.last_background_error = Some(message);
            }

            AppEvent::TokenLookupStarted { address } => {
                self.state.search_status = state::SearchStatus::LoadingState;
                self.state.candles.clear();
                self.state.selected_token = None;
                tracing::info!(%address, "buscando token");
            }
            AppEvent::TokenStateResolved { view } => {
                self.state.selected_token = Some(*view);
                self.state.search_status = state::SearchStatus::LoadingHistory;
            }
            AppEvent::TokenHistoryResolved { view, candles } => {
                self.state.selected_token = Some(*view);
                self.state.candles = candles;
                self.state.search_status = state::SearchStatus::Done;
            }
            AppEvent::TokenLookupFailed { message } => {
                self.state.search_status = state::SearchStatus::Failed(message);
            }

            AppEvent::SearchInputChar(c) => {
                // Una dirección EVM son 42 caracteres; más allá es basura
                // pegada por error.
                if self.state.search_input.len() < 64 {
                    self.state.search_input.push(c);
                }
            }
            AppEvent::SearchInputBackspace => {
                self.state.search_input.pop();
            }
            AppEvent::SearchInputClear => {
                self.state.search_input.clear();
            }
            AppEvent::TabSelected(tab) => {
                self.state.active_tab = tab;
            }
            AppEvent::Quit => {
                self.state.should_quit = true;
            }
            AppEvent::Tick => {
                self.state.search_focused = !self.state.search_focused;
            }
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
