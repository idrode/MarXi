//! Datos puros que la UI lee para renderizar. Sin lógica de red aquí.

#[derive(Debug, Default)]
pub struct AppState {
    pub active_tab: Tab,

    pub last_block_seen: Option<u64>,
    pub last_background_error: Option<String>,
    pub last_trade_message: Option<(bool, String)>,

    /// Lo que el usuario lleva tecleado en el buscador por-token.
    pub search_input: String,
    /// Estado de la última búsqueda, para que el panel diga qué está pasando
    /// en vez de quedarse en blanco mientras el backfill trabaja.
    pub search_status: SearchStatus,
    /// Velas del token seleccionado, ya agregadas.
    pub candles: Vec<crate::data::candles::Candle>,
    /// `true` mientras el cursor debe parpadear en el campo de búsqueda.
    pub search_focused: bool,
    /// Señal para que el render loop termine.
    pub should_quit: bool,

    /// (launchpad, token_address) — más reciente al final; la UI decide cuántos mostrar.
    pub recent_launches: Vec<(String, String)>,
    /// (token_address, pool_id)
    pub recent_graduations: Vec<(String, String)>,

    pub selected_token: Option<TokenView>,
    pub positions: Vec<Position>,

    // --- operator_tracker (pestaña Operador) ---
    /// Última wallet consultada como operador. La consulta la dispara el
    /// mismo campo del buscador: si la dirección no es un launch de Pons V2,
    /// se reintenta por esta vía.
    pub operator: Option<OperatorView>,
    pub operator_status: OperatorStatus,
    /// Estado de la carga de financiación, que va aparte porque es la parte
    /// cara (minutos) y se pide a mano con `f`.
    pub funding_status: FundingStatus,

    // --- señal 2 en vivo (pestaña Alertas) ---
    pub watch: WatchState,
    /// Alertas de esta sesión, más reciente al final. **Acotada**: sin tope
    /// sería la misma fuga que `recent_launches` (deuda nº3).
    pub alerts: Vec<LiveAlert>,
    /// Alertas que aún no se han mirado en la pestaña Alertas. El contador de
    /// la barra de estado lo lee para que se vea desde cualquier pestaña.
    pub alerts_unseen: usize,
}

/// Tope de alertas guardadas en memoria. Las que se caen por arriba siguen
/// en la base de datos: `cargo run -- alert list` es el registro completo,
/// esto solo es la ventana visible de la sesión.
pub const MAX_LIVE_ALERTS: usize = 200;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Buscador por-token: pegar una dirección y ver su estado y su gráfico.
    /// Es la pestaña de arranque porque es el flujo que el usuario quiere
    /// usar primero (ver CLAUDE.md, "Cambio de prioridad").
    #[default]
    Search,
    /// Perfil de una wallet como operador de launchpad: clasificación,
    /// historial de lanzamientos, financiación y si está en la watchlist.
    Operator,
    /// Alertas de la señal 2 recibidas en vivo durante esta sesión.
    Alerts,
    Dashboard,
    Sniper,
    TokenDetail,
    Positions,
    Settings,
}

impl Tab {
    pub const ORDER: [Tab; 8] = [
        Tab::Search,
        Tab::Operator,
        Tab::Alerts,
        Tab::Dashboard,
        Tab::Sniper,
        Tab::TokenDetail,
        Tab::Positions,
        Tab::Settings,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Search => "Buscador",
            Tab::Operator => "Operador",
            Tab::Alerts => "Alertas",
            Tab::Dashboard => "Dashboard",
            Tab::Sniper => "Sniper",
            Tab::TokenDetail => "Detalle",
            Tab::Positions => "Posiciones",
            Tab::Settings => "Ajustes",
        }
    }

    pub fn next(&self) -> Tab {
        let i = Self::ORDER.iter().position(|t| t == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }

    pub fn prev(&self) -> Tab {
        let i = Self::ORDER.iter().position(|t| t == self).unwrap_or(0);
        Self::ORDER[(i + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }
}

/// En qué punto está la consulta del buscador. El backfill tarda decenas de
/// segundos, así que el panel tiene que poder decirlo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SearchStatus {
    #[default]
    Idle,
    /// Consultando estado on-chain (rápido).
    LoadingState,
    /// Trayendo histórico de eventos (lento).
    LoadingHistory,
    Done,
    Failed(String),
}

impl SearchStatus {
    pub fn is_loading(&self) -> bool {
        matches!(self, SearchStatus::LoadingState | SearchStatus::LoadingHistory)
    }
}

/// Snapshot de lo que sabemos de un token para pintarlo en el panel principal.
///
/// Lo rellena `data::token_lookup::lookup` con una consulta bajo demanda
/// (estado on-chain), y `data::backfill` completa después lo que sale del
/// histórico de eventos (velas, holders, variación). Todos los precios e
/// importes están **denominados en el `pairToken` del launch**, que puede no
/// ser ETH: Pons V2 admite stock tokens y USDG como par (ver CLAUDE.md,
/// "Multi-quote-asset"). Por eso `pair_symbol`/`pair_decimals` acompañan
/// siempre a cualquier cifra.
#[derive(Debug, Clone, Default)]
pub struct TokenView {
    pub address: String,
    pub symbol: Option<String>,
    #[allow(dead_code)] // multi-launchpad (Fase 5); hoy solo Pons V2
    pub launchpad: Option<String>,
    pub phase: TokenPhase,
    #[allow(dead_code)] // honeypot check (Fase 2)
    pub honeypot_checked: bool,
    #[allow(dead_code)] // honeypot check (Fase 2)
    pub honeypot_risk: Option<String>,

    // --- estado on-chain (data::token_lookup) ---
    pub decimals: Option<u8>,
    /// Supply ya escalado por decimals.
    pub total_supply: Option<f64>,
    /// Dirección del pairToken. `0x00..00` significa ETH nativo.
    pub pair_address: Option<String>,
    pub pair_symbol: Option<String>,
    pub pair_decimals: Option<u8>,
    /// Contrato de bonding curve propio de este token.
    pub curve_address: Option<String>,
    /// Solo si graduó: PoolId de Uniswap V4 (keccak del PoolKey).
    pub pool_id: Option<String>,
    /// Precio de 1 token en unidades de `pairToken`.
    pub price_in_pair: Option<f64>,
    /// `price_in_pair * total_supply`, en unidades de `pairToken`.
    pub market_cap_in_pair: Option<f64>,
    /// Liquidez del pool V4 (unidades crudas de V4), solo si graduó.
    pub pool_liquidity: Option<u128>,
    /// Progreso hacia la graduación en tanto por uno, solo en curva.
    pub graduation_progress: Option<f64>,
    /// Bloque desde el que tiene sentido buscar su histórico de eventos.
    pub first_event_block: Option<u64>,

    // --- derivado del histórico (data::backfill) ---
    /// Direcciones con saldo > 0 reconstruidas desde los `Transfer`.
    pub holders: Option<usize>,
    /// Variación porcentual entre el primer trade indexado y el precio actual.
    pub change_since_first_trade_pct: Option<f64>,
    /// Número de trades (swaps o trades de curva) indexados.
    pub trades_indexed: Option<usize>,
}

/// Fases reales de `GraduationPhase` en el fuente de Pons V2, más `Unknown`
/// para lo que aún no se ha consultado. El valor numérico es el del enum
/// on-chain: NotGraduated 0, Swept 1, PoolCreated 2, Rescued 3.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TokenPhase {
    #[default]
    Unknown,
    /// `NotGraduated` (0): se compra y vende contra la curva.
    BondingCurve,
    /// `Swept` (1): la curva se vació pero el pool V4 aún no existe.
    /// Estado transitorio dentro de la graduación, no operable por ninguna
    /// de las dos vías.
    Swept,
    /// `PoolCreated` (2): opera en el pool de Uniswap V4.
    Graduated,
    /// `Rescued` (3): la graduación falló y fue rescatada manualmente.
    Rescued,
}

impl TokenPhase {
    pub fn from_onchain(phase: u8) -> Self {
        match phase {
            0 => Self::BondingCurve,
            1 => Self::Swept,
            2 => Self::Graduated,
            3 => Self::Rescued,
            _ => Self::Unknown,
        }
    }

    /// Etiqueta corta para la UI y el CLI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "desconocida",
            Self::BondingCurve => "bonding curve",
            Self::Swept => "swept (graduando)",
            Self::Graduated => "graduado (Uniswap V4)",
            Self::Rescued => "rescatado",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Position {
    pub token_address: String,
    pub entry_price: f64,
    pub amount: f64,
    #[allow(dead_code)] // cierre TP/SL (Fase 3)
    pub take_profit: Option<f64>,
    #[allow(dead_code)] // cierre TP/SL (Fase 3)
    pub stop_loss: Option<f64>,
}

// ---------------------------------------------------------------------------
// operator_tracker en la TUI
// ---------------------------------------------------------------------------

/// En qué punto está la consulta de un operador. Separado de `SearchStatus`
/// porque los dos flujos comparten campo de entrada pero no estado: una
/// consulta puede fallar como token y estar cargando como operador.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OperatorStatus {
    #[default]
    Idle,
    /// Comprobando si la dirección es una wallet o un contrato.
    Classifying,
    /// Trayendo el historial de `TokenLaunched` por `topics[3]`.
    LoadingHistory,
    Done,
    Failed(String),
}

impl OperatorStatus {
    pub fn is_loading(&self) -> bool {
        matches!(self, OperatorStatus::Classifying | OperatorStatus::LoadingHistory)
    }
}

/// Estado de la carga de financiación de un operador.
///
/// Va aparte del resto del perfil porque su coste es de otro orden: el
/// `Transfer` de ERC-20 se pide **sin filtro de `address`** sobre toda la
/// chain (es la consulta que agotaba el RPC público antes de la subdivisión
/// de tramos) y el nativo va por bisección de saldo. Son minutos, no
/// segundos, así que no se lanza sola: la pide el usuario con `f`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FundingStatus {
    #[default]
    NotRequested,
    Loading,
    Done,
    Failed(String),
}

/// Lo que la pestaña Operador pinta. Igual que `TokenView`, es un DTO: lo
/// rellenan las tareas de fondo y la UI solo lo lee.
#[derive(Debug, Clone, Default)]
pub struct OperatorView {
    pub address: String,
    /// `DeployerKind::label()`: wallet, wallet con delegación 7702, posible
    /// relay o sin comprobar.
    pub kind_label: String,
    /// `true` si la clasificación pide aviso destacado (relay o no evaluado).
    pub kind_needs_warning: bool,
    /// `true` solo si es wallet o wallet delegada.
    // Se rellena en `ui::` pero nadie lo lee; quitarlo exige tocar `src/ui/`
    // (TUI pausada): decidir al retomarla.
    #[allow(dead_code)]
    pub kind_is_wallet: bool,

    /// Está en `watchlist.toml`, con su etiqueta si la tiene.
    pub in_watchlist: bool,
    pub watchlist_label: Option<String>,

    pub launches: usize,
    /// Mediana de segundos entre lanzamientos consecutivos.
    pub median_interval_secs: Option<u64>,
    /// `(pairToken, veces)`, de más usado a menos.
    pub pair_tokens: Vec<(String, usize)>,
    pub history_from_block: u64,
    pub history_to_block: u64,
    /// Últimos lanzamientos, más reciente al final.
    pub recent_launches: Vec<OperatorLaunchRow>,

    /// De cuándo era el snapshot cacheado que había antes de esta consulta.
    /// La caché **no se sirve sola** (decisión del 2026-09-18): esto es
    /// informativo, el dato mostrado siempre viene de la chain.
    pub cached_at: Option<u64>,

    // --- financiación, solo si se pidió ---
    pub fundings_native: usize,
    pub fundings_erc20: usize,
    /// Entradas que el criterio de las cuatro condiciones considera
    /// financiación de arranque, ya clasificadas por confianza.
    pub startup_high: usize,
    pub startup_low: usize,
    pub income: usize,
    pub not_evaluated: usize,
    /// Las de arranque, en texto ya formateado para la tabla.
    pub startup_rows: Vec<String>,
    /// Resumen del baseline por activo, o vacío si no hay muestra suficiente.
    pub baseline_rows: Vec<String>,
    /// `true` si el escaneo nativo llegó a su tope: el recuento es un suelo.
    pub funding_truncated: bool,
}

#[derive(Debug, Clone)]
pub struct OperatorLaunchRow {
    pub block: u64,
    pub timestamp: u64,
    pub token: String,
    pub pair_token: String,
}

/// Estado de la vigilancia en vivo de la señal 2.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WatchState {
    /// No se ha intentado arrancar (no debería verse: el arranque es
    /// automático si hay watchlist).
    #[default]
    Idle,
    /// La watchlist está vacía, así que no hay nada que vigilar. No es un
    /// error: es el caso normal la primera vez.
    NoWatchlist,
    Starting,
    Running {
        watched: usize,
        /// `WatchTransport::label()`: WebSocket o sondeo.
        transport: String,
        from_block: u64,
    },
    Failed(String),
}

/// Una alerta de la señal 2 tal y como se pinta.
#[derive(Debug, Clone)]
pub struct LiveAlert {
    /// `id` en la tabla `alert`, que es con el que se cierra el ciclo de
    /// prueba y error (`cargo run -- alert <id> <outcome>`). `None` si la
    /// base falló: la alerta se muestra igual, marcada como no persistida.
    pub alert_id: Option<i64>,
    pub received_at: u64,
    pub deployer: String,
    pub label: Option<String>,
    pub token: String,
    pub pair_token: String,
    pub graduation_threshold: String,
    pub block: u64,
    pub tx_hash: String,
}
