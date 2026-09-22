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

    // --- operator_tracker: perfil de una wallet (pestaña Operador) ---
    /// La dirección no era un launch de Pons V2 y se reintenta como operador.
    /// Solo se llega aquí desde `LookupError::NotAPonsV2Launch`: un RPC caído
    /// no se reinterpreta como "será una wallet".
    OperatorLookupStarted { address: String },
    /// Clasificación resuelta (wallet / delegada 7702 / posible relay). Llega
    /// antes que el historial porque son un par de `eth_call`.
    OperatorClassified { view: Box<state::OperatorView> },
    /// Historial de `TokenLaunched` del deployer resuelto.
    OperatorHistoryResolved { view: Box<state::OperatorView> },
    OperatorLookupFailed { message: String },
    /// El usuario pidió la financiación con `f`. Es la parte cara.
    OperatorFundingsStarted,
    OperatorFundingsResolved { view: Box<state::OperatorView> },
    OperatorFundingsFailed { message: String },
    /// Alta/baja en `watchlist.toml` desde la pestaña Operador.
    WatchlistChanged { in_watchlist: bool, message: String },

    // --- señal 2 en vivo ---
    OperatorWatchStarted {
        watched: usize,
        transport: crate::data::operator_tracker::watcher::WatchTransport,
        from_block: u64,
    },
    OperatorWatchFailed { message: String },
    /// **La señal 2**: una wallet de la watchlist ha lanzado un token.
    /// `alert_id` es `None` si no se pudo escribir en la base; la alerta se
    /// muestra igual.
    OperatorLaunchDetected {
        alert_id: Option<i64>,
        deployer: String,
        label: Option<String>,
        token: String,
        pair_token: String,
        graduation_threshold: String,
        block: u64,
        tx_hash: String,
    },
    /// El usuario entró en la pestaña Alertas: se da por leído el contador.
    AlertsSeen,

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

            AppEvent::OperatorLookupStarted { address } => {
                self.state.operator_status = state::OperatorStatus::Classifying;
                self.state.funding_status = state::FundingStatus::NotRequested;
                self.state.operator = None;
                // La consulta de token ya falló; que su error no se quede
                // pintado como si siguiera vigente.
                self.state.search_status = state::SearchStatus::Idle;
                self.state.active_tab = state::Tab::Operator;
                tracing::info!(%address, "consultando wallet como operador");
            }
            AppEvent::OperatorClassified { view } => {
                self.state.operator = Some(*view);
                self.state.operator_status = state::OperatorStatus::LoadingHistory;
            }
            AppEvent::OperatorHistoryResolved { view } => {
                self.state.operator = Some(*view);
                self.state.operator_status = state::OperatorStatus::Done;
            }
            AppEvent::OperatorLookupFailed { message } => {
                self.state.operator_status = state::OperatorStatus::Failed(message);
            }
            AppEvent::OperatorFundingsStarted => {
                self.state.funding_status = state::FundingStatus::Loading;
            }
            AppEvent::OperatorFundingsResolved { view } => {
                self.state.operator = Some(*view);
                self.state.funding_status = state::FundingStatus::Done;
            }
            AppEvent::OperatorFundingsFailed { message } => {
                self.state.funding_status = state::FundingStatus::Failed(message);
            }
            AppEvent::WatchlistChanged { in_watchlist, message } => {
                if let Some(op) = self.state.operator.as_mut() {
                    op.in_watchlist = in_watchlist;
                    if !in_watchlist {
                        op.watchlist_label = None;
                    }
                }
                self.state.last_trade_message = Some((true, message));
            }

            AppEvent::OperatorWatchStarted { watched, transport, from_block } => {
                tracing::info!(watched, transport = transport.label(), from_block, "vigilancia de la señal 2 activa");
                self.state.watch = state::WatchState::Running {
                    watched,
                    transport: transport.label().to_string(),
                    from_block,
                };
            }
            AppEvent::OperatorWatchFailed { message } => {
                tracing::warn!(%message, "la vigilancia de la señal 2 no está activa");
                self.state.watch = state::WatchState::Failed(message);
            }
            AppEvent::OperatorLaunchDetected {
                alert_id,
                deployer,
                label,
                token,
                pair_token,
                graduation_threshold,
                block,
                tx_hash,
            } => {
                self.state.alerts.push(state::LiveAlert {
                    alert_id,
                    received_at: now_secs(),
                    deployer,
                    label,
                    token,
                    pair_token,
                    graduation_threshold,
                    block,
                    tx_hash,
                });
                // Poda: la base guarda el registro completo, esto es solo la
                // ventana visible (deuda nº3, no repetirla).
                if self.state.alerts.len() > state::MAX_LIVE_ALERTS {
                    let exceso = self.state.alerts.len() - state::MAX_LIVE_ALERTS;
                    self.state.alerts.drain(0..exceso);
                }
                self.state.alerts_unseen += 1;
            }
            AppEvent::AlertsSeen => {
                self.state.alerts_unseen = 0;
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
                if tab == state::Tab::Alerts {
                    self.state.alerts_unseen = 0;
                }
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

/// Segundos desde epoch. Aquí y no en la tarea de fondo porque el instante
/// que interesa es el de aplicar el evento, que es lo que se pinta.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
