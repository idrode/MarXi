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
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Buscador por-token: pegar una dirección y ver su estado y su gráfico.
    /// Es la pestaña de arranque porque es el flujo que el usuario quiere
    /// usar primero (ver CLAUDE.md, "Cambio de prioridad").
    #[default]
    Search,
    Dashboard,
    Sniper,
    TokenDetail,
    Positions,
    Settings,
}

impl Tab {
    pub const ORDER: [Tab; 6] = [
        Tab::Search,
        Tab::Dashboard,
        Tab::Sniper,
        Tab::TokenDetail,
        Tab::Positions,
        Tab::Settings,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Search => "Buscador",
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
    pub launchpad: Option<String>,
    pub phase: TokenPhase,
    pub honeypot_checked: bool,
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
    pub take_profit: Option<f64>,
    pub stop_loss: Option<f64>,
}
