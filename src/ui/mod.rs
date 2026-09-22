//! Loop de render con ratatui. Puramente síncrono: lee `AppState`, dibuja,
//! espera input de teclado o el siguiente `AppEvent`. Ninguna llamada de red
//! ni de disco ocurre aquí — eso vive en tareas tokio spawneadas que
//! comunican por el canal `mpsc`.
//!
//! Invariante del proyecto que se respeta aquí: **solo `App::apply_event`
//! muta `AppState`**. Por eso hasta las teclas se convierten en `AppEvent`
//! antes de tocar nada, en vez de escribir en el estado desde el loop.

pub mod alerts;
pub mod dashboard;
pub mod operator;
pub mod positions;
pub mod search;
pub mod settings;
pub mod sniper;
pub mod token_detail;

use crate::app::{state::Tab, App, AppEvent};
use crate::chain::ChainProvider;
use crate::config::AppConfig;
use crate::data::operator_tracker::watchlist::{Watchlist, DEFAULT_WATCHLIST_PATH};
use crate::data::token_lookup::LookupAddresses;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{Frame, Terminal};
use std::sync::Arc;
use std::time::Duration;

/// Cada cuánto se repinta cuando no pasa nada. Suficiente para que el cursor
/// del campo de búsqueda parpadee sin gastar CPU.
const TICK: Duration = Duration::from_millis(250);

/// Arranca la TUI. Toma posesión de la terminal y la restaura al salir,
/// incluso si el cuerpo del loop devuelve error.
pub async fn run(mut app: App, cfg: AppConfig) -> anyhow::Result<()> {
    let provider = Arc::new(ChainProvider::connect(&cfg.chain).await?);
    let addrs = Arc::new(LookupAddresses::from_config(&cfg)?);
    let cfg = Arc::new(cfg);

    // Las teclas se leen en un hilo aparte: `event::read` bloquea, y el
    // render loop no puede bloquear.
    // Si `event::read()` falla, el hilo no puede seguir leyendo teclado: se
    // avisa por el canal de eventos (deuda nº14 — nunca morir en silencio) y
    // se pide salir, porque una TUI sin teclado no tiene forma de cerrarse.
    let (key_tx, mut key_rx) = tokio::sync::mpsc::channel::<KeyEvent>(64);
    let err_tx = app.event_tx.clone();
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                if key_tx.blocking_send(k).is_err() {
                    // El receptor se cerró: la app ya está saliendo.
                    break;
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::error!("el hilo de teclado se detiene: event::read() falló: {e}");
                let _ = err_tx.blocking_send(AppEvent::BackgroundError {
                    source: "teclado".to_string(),
                    message: format!("event::read() falló, se pierde el teclado: {e}"),
                });
                let _ = err_tx.blocking_send(AppEvent::Quit);
                break;
            }
        }
    });

    // Vigilancia de la señal 2: automática si hay watchlist. Que hubiera que
    // activarla a mano es justo la forma de perderse un lanzamiento mientras
    // el programa está abierto, que es el caso de uso.
    let mut watch_handle = start_watcher(&app, &provider, &addrs, &cfg);

    let mut terminal = init_terminal()?;
    let result = event_loop(
        &mut terminal,
        &mut app,
        &mut key_rx,
        &provider,
        &addrs,
        &cfg,
        &mut watch_handle,
    )
    .await;
    restore_terminal()?;
    if let Some(h) = watch_handle {
        h.abort();
    }
    result
}

type Tui = Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

async fn event_loop(
    terminal: &mut Tui,
    app: &mut App,
    key_rx: &mut tokio::sync::mpsc::Receiver<KeyEvent>,
    provider: &Arc<ChainProvider>,
    addrs: &Arc<LookupAddresses>,
    cfg: &Arc<AppConfig>,
    watch_handle: &mut Option<tokio::task::JoinHandle<()>>,
) -> anyhow::Result<()> {
    let mut blink = std::time::Instant::now();

    loop {
        terminal.draw(|f| draw(f, app))?;
        if app.state.should_quit {
            return Ok(());
        }

        tokio::select! {
            Some(key) = key_rx.recv() => {
                // Dos teclas tocan disco o la tarea de vigilancia y no se
                // pueden expresar como un `AppEvent` puro: se resuelven aquí
                // y lo que cambia el estado sigue pasando por `apply_event`.
                if handle_local_key(key, app, provider, addrs, cfg, watch_handle) {
                    continue;
                }
                for ev in map_key(key, app) {
                    dispatch(ev, app, provider, addrs, cfg);
                }
            }
            Some(ev) = app.event_rx.recv() => {
                // Por aquí llegan también eventos que disparan trabajo, no
                // solo resultados: el más importante es el fallback
                // token → operador, que lo emite la propia tarea de búsqueda.
                // Por eso el despacho está en un sitio y no en la rama de
                // teclado (que es donde estaba, y el fallback se quedaba
                // colgado sin que nadie lanzara la consulta).
                dispatch(ev, app, provider, addrs, cfg);
            }
            _ = tokio::time::sleep(TICK) => {
                if blink.elapsed() >= Duration::from_millis(500) {
                    blink = std::time::Instant::now();
                    app.apply_event(AppEvent::Tick);
                }
            }
        }
    }
}

/// Traduce una tecla a los `AppEvent` que corresponda. Devuelve una lista
/// porque una tecla puede provocar más de un cambio de estado.
fn map_key(key: KeyEvent, app: &App) -> Vec<AppEvent> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => vec![AppEvent::Quit],
        KeyCode::Esc => vec![AppEvent::Quit],
        KeyCode::Tab => vec![AppEvent::TabSelected(app.state.active_tab.next())],
        KeyCode::BackTab => vec![AppEvent::TabSelected(app.state.active_tab.prev())],
        // Pestaña Operador: `f` pide la financiación (la parte cara).
        KeyCode::Char('f') if app.state.active_tab == Tab::Operator => {
            match (&app.state.operator, &app.state.funding_status) {
                (Some(_), crate::app::state::FundingStatus::Loading) => vec![],
                (Some(_), _) => vec![AppEvent::OperatorFundingsStarted],
                (None, _) => vec![],
            }
        }
        // `r` recarga el perfil del operador que está en pantalla.
        KeyCode::Char('r') if app.state.active_tab == Tab::Operator => {
            match &app.state.operator {
                Some(v) => vec![AppEvent::OperatorLookupStarted { address: v.address.clone() }],
                None => vec![],
            }
        }
        _ if app.state.active_tab != Tab::Search => vec![],
        KeyCode::Char('u') if ctrl => vec![AppEvent::SearchInputClear],
        KeyCode::Backspace => vec![AppEvent::SearchInputBackspace],
        KeyCode::Char(c) => vec![AppEvent::SearchInputChar(c)],
        KeyCode::Enter => {
            let addr = app.state.search_input.trim().to_string();
            if addr.is_empty() {
                vec![]
            } else {
                vec![AppEvent::TokenLookupStarted { address: addr }]
            }
        }
        _ => vec![],
    }
}

/// Lanza la consulta en su propia tarea. Emite el estado en cuanto lo tiene y
/// el histórico después, para que el panel no se quede vacío los segundos que
/// tarda el backfill.
fn spawn_lookup(
    address: String,
    tx: tokio::sync::mpsc::Sender<AppEvent>,
    provider: Arc<ChainProvider>,
    addrs: Arc<LookupAddresses>,
    cfg: Arc<AppConfig>,
) {
    tokio::spawn(async move {
        let token = match address.trim().parse::<alloy::primitives::Address>() {
            Ok(t) => t,
            Err(e) => {
                let _ = tx
                    .send(AppEvent::TokenLookupFailed {
                        message: format!("{address:?} no es una dirección EVM válida: {e}"),
                    })
                    .await;
                return;
            }
        };

        let view = match crate::data::token_lookup::lookup(&provider, &addrs, token).await {
            Ok(v) => v,
            Err(e) => {
                // **Solo** si sabemos que no es un launch de Pons V2 se
                // reintenta como operador. Un RPC caído no se reinterpreta
                // como "será una wallet": eso convertiría un fallo de red en
                // una consulta cara y en un diagnóstico equivocado.
                let no_es_token = e
                    .downcast_ref::<crate::data::token_lookup::LookupError>()
                    .map(|le| matches!(le, crate::data::token_lookup::LookupError::NotAPonsV2Launch { .. }))
                    .unwrap_or(false);
                let ev = if no_es_token {
                    AppEvent::OperatorLookupStarted { address: address.clone() }
                } else {
                    AppEvent::TokenLookupFailed { message: format!("{e}") }
                };
                let _ = tx.send(ev).await;
                return;
            }
        };
        let _ = tx.send(AppEvent::TokenStateResolved { view: Box::new(view.clone()) }).await;

        let mut view = view;
        match crate::data::backfill::backfill(
            &provider,
            addrs.pool_manager,
            addrs.factory,
            token,
            &view,
            cfg.indexer.backfill_max_blocks_per_request,
            crate::cli::DEFAULT_CANDLE_SECONDS,
            true,
        )
        .await
        {
            Ok(h) => {
                view.holders = h.holders;
                view.trades_indexed = Some(h.trades.len());
                view.change_since_first_trade_pct = h.change_pct(view.price_in_pair);
                view.first_event_block = h.launch_block;
                let _ = tx
                    .send(AppEvent::TokenHistoryResolved {
                        view: Box::new(view),
                        candles: h.candles,
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(AppEvent::TokenLookupFailed {
                        message: format!("el estado se leyó bien pero el histórico falló: {e}"),
                    })
                    .await;
            }
        }
    });
}

fn init_terminal() -> anyhow::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;
    Ok(terminal)
}

fn restore_terminal() -> anyhow::Result<()> {
    disable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

pub fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(5), Constraint::Length(1)])
        .split(frame.area());

    draw_tabs(frame, app, chunks[0]);

    // Cada pestaña dibuja dentro del área central.
    let inner = chunks[1];
    let sub = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1)])
        .split(inner);
    let _ = sub;
    draw_tab_body(frame, app, inner);

    draw_status(frame, app, chunks[2]);
}

fn draw_tab_body(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    // Las pestañas heredadas dibujan a pantalla completa; se les acota el
    // área con un buffer intermedio no es posible sin reescribirlas, así que
    // por ahora solo el buscador respeta el layout. El resto son esqueleto.
    match app.state.active_tab {
        Tab::Search => search::draw_in(frame, app, area),
        Tab::Operator => operator::draw_in(frame, app, area),
        Tab::Alerts => alerts::draw_in(frame, app, area),
        Tab::Dashboard => dashboard::draw(frame, app),
        Tab::Sniper => sniper::draw(frame, app),
        Tab::TokenDetail => token_detail::draw(frame, app),
        Tab::Positions => positions::draw(frame, app),
        Tab::Settings => settings::draw(frame, app),
    }
}

fn draw_tabs(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mut spans = Vec::new();
    for t in Tab::ORDER {
        let active = t == app.state.active_tab;
        // La pestaña de alertas lleva el contador de no leídas pegado al
        // título: es lo que hace que se vea sin estar mirándola.
        let titulo = if t == Tab::Alerts && app.state.alerts_unseen > 0 {
            format!(" {} ({}) ", t.title(), app.state.alerts_unseen)
        } else {
            format!(" {} ", t.title())
        };
        spans.push(Span::styled(
            titulo,
            if active {
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_status(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let s = &app.state;
    let mut parts = vec!["tab/shift-tab cambia panel".to_string(), "esc sale".to_string()];

    // Estado de la vigilancia, visible desde cualquier pestaña: si no está
    // mirando, hay que enterarse sin ir a buscarlo.
    match &s.watch {
        crate::app::state::WatchState::Running { watched, .. } => {
            parts.push(format!("vigilando {watched}"))
        }
        crate::app::state::WatchState::Failed(_) => parts.push("vigilancia parada".to_string()),
        _ => {}
    }
    if let Some(b) = s.last_block_seen {
        parts.push(format!("bloque {b}"));
    }
    if let Some((_, m)) = &s.last_trade_message {
        parts.push(m.clone());
    }
    if let Some(e) = &s.last_background_error {
        parts.push(format!("error: {e}"));
    }

    let mut spans = Vec::new();
    if s.alerts_unseen > 0 {
        spans.push(Span::styled(
            format!(" ⚑ {} alerta(s) ", s.alerts_unseen),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(255, 193, 7))
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(parts.join("  ·  "), Style::default().fg(Color::DarkGray)));

    frame.render_widget(Paragraph::new(Line::from(spans)).block(Block::default()), area);
}

/// Único sitio donde un `AppEvent` puede provocar trabajo de fondo.
///
/// Da igual si el evento viene del teclado o del canal: los eventos que
/// *piden* algo (`...Started`) lanzan su tarea aquí y después se aplican como
/// cualquier otro. La invariante se mantiene — `apply_event` sigue siendo lo
/// único que muta `AppState`.
fn dispatch(
    ev: AppEvent,
    app: &mut App,
    provider: &Arc<ChainProvider>,
    addrs: &Arc<LookupAddresses>,
    cfg: &Arc<AppConfig>,
) {
    match &ev {
        AppEvent::TokenLookupStarted { address } => spawn_lookup(
            address.clone(),
            app.event_tx.clone(),
            provider.clone(),
            addrs.clone(),
            cfg.clone(),
        ),
        AppEvent::OperatorLookupStarted { address } => spawn_operator_lookup(
            address.clone(),
            app.event_tx.clone(),
            provider.clone(),
            addrs.clone(),
            cfg.clone(),
        ),
        AppEvent::OperatorFundingsStarted => {
            if let Some(v) = &app.state.operator {
                spawn_fundings(
                    v.address.clone(),
                    app.event_tx.clone(),
                    provider.clone(),
                    addrs.clone(),
                    cfg.clone(),
                );
            }
        }
        _ => {}
    }
    app.apply_event(ev);
}

/// Teclas que no se pueden expresar como un `AppEvent` puro porque tocan
/// disco (`watchlist.toml`) o la tarea de vigilancia. Devuelve `true` si la
/// tecla se consumió aquí.
///
/// La invariante se mantiene: lo que cambia `AppState` se sigue emitiendo
/// como evento y aplicando por `apply_event`.
fn handle_local_key(
    key: KeyEvent,
    app: &mut App,
    provider: &Arc<ChainProvider>,
    addrs: &Arc<LookupAddresses>,
    cfg: &Arc<AppConfig>,
    watch_handle: &mut Option<tokio::task::JoinHandle<()>>,
) -> bool {
    match (key.code, app.state.active_tab) {
        (KeyCode::Char('w'), Tab::Operator) => {
            let ev = toggle_watchlist(app);
            app.apply_event(ev);
            true
        }
        // `R` (mayúscula) recarga la watchlist y rearranca la vigilancia, para
        // no tener que reiniciar el programa tras añadir una wallet.
        (KeyCode::Char('R'), Tab::Alerts) | (KeyCode::Char('v'), Tab::Alerts) => {
            let parar = key.code == KeyCode::Char('v')
                && matches!(app.state.watch, crate::app::state::WatchState::Running { .. });
            if let Some(h) = watch_handle.take() {
                h.abort();
            }
            if parar {
                app.apply_event(AppEvent::OperatorWatchFailed {
                    message: "parada a mano (v para volver a arrancar)".to_string(),
                });
            } else {
                *watch_handle = start_watcher(app, provider, addrs, cfg);
            }
            true
        }
        _ => false,
    }
}

/// Alta o baja de la wallet en pantalla en `watchlist.toml`.
///
/// Misma regla que `cargo run -- watch`: un `PossibleRelayContract` **no
/// entra** sin decisión explícita. Aquí no hay `--force`, así que se rechaza
/// y se remite al comando: el coste asimétrico es el motivo (un relay con
/// miles de lanzamientos taparía el resto de alertas), y no quiero que una
/// tecla suelta pueda provocarlo.
fn toggle_watchlist(app: &App) -> AppEvent {
    let Some(v) = &app.state.operator else {
        return AppEvent::WatchlistChanged {
            in_watchlist: false,
            message: "no hay ninguna wallet en pantalla.".to_string(),
        };
    };
    let addr = match v.address.parse::<alloy::primitives::Address>() {
        Ok(a) => a,
        Err(e) => {
            return AppEvent::WatchlistChanged {
                in_watchlist: v.in_watchlist,
                message: format!("dirección inválida: {e}"),
            }
        }
    };

    let mut list = match Watchlist::load(DEFAULT_WATCHLIST_PATH) {
        Ok(l) => l,
        Err(e) => {
            return AppEvent::WatchlistChanged {
                in_watchlist: v.in_watchlist,
                message: format!("no se pudo leer {DEFAULT_WATCHLIST_PATH}: {e}"),
            }
        }
    };

    let quitar = list.contains(addr);
    if !quitar && v.kind_needs_warning {
        return AppEvent::WatchlistChanged {
            in_watchlist: false,
            message: format!(
                "{} no está confirmada como wallet: añádela con `cargo run -- watch {} --force` si estás seguro.",
                v.kind_label, v.address
            ),
        };
    }

    if quitar {
        list.remove(addr);
    } else {
        list.add(addr, None, Some("añadida desde la TUI".to_string()));
    }

    match list.save(DEFAULT_WATCHLIST_PATH) {
        Ok(()) => AppEvent::WatchlistChanged {
            in_watchlist: !quitar,
            message: format!(
                "{} {} la watchlist. Pulsa R en Alertas para que la vigilancia la coja.",
                v.address,
                if quitar { "fuera de" } else { "añadida a" }
            ),
        },
        Err(e) => AppEvent::WatchlistChanged {
            in_watchlist: quitar,
            message: format!("no se pudo guardar {DEFAULT_WATCHLIST_PATH}: {e}"),
        },
    }
}

/// Arranca la vigilancia de la señal 2 si hay watchlist. Devuelve `None` si
/// no hay nada que vigilar, que no es un error: es el caso normal la primera
/// vez que se abre el programa.
fn start_watcher(
    app: &App,
    provider: &Arc<ChainProvider>,
    addrs: &Arc<LookupAddresses>,
    cfg: &Arc<AppConfig>,
) -> Option<tokio::task::JoinHandle<()>> {
    let tx = app.event_tx.clone();
    let list = match Watchlist::load(DEFAULT_WATCHLIST_PATH) {
        Ok(l) => l,
        Err(e) => {
            let _ = tx.try_send(AppEvent::OperatorWatchFailed {
                message: format!("{DEFAULT_WATCHLIST_PATH} no se pudo leer: {e}"),
            });
            return None;
        }
    };
    if list.operators.is_empty() {
        let _ = tx.try_send(AppEvent::OperatorWatchFailed {
            message: "watchlist vacía: no hay nada que vigilar.".to_string(),
        });
        return None;
    }
    Some(crate::data::operator_tracker::spawn_launch_watcher(
        provider.clone(),
        addrs.factory,
        list,
        cfg.indexer.db_path.clone(),
        tx,
        cfg.indexer.backfill_max_blocks_per_request,
    ))
}

/// Consulta una dirección **como operador**: clasificación primero (barata) y
/// luego el historial de lanzamientos.
///
/// La financiación no se pide aquí a propósito: ver `FundingStatus`.
fn spawn_operator_lookup(
    address: String,
    tx: tokio::sync::mpsc::Sender<AppEvent>,
    provider: Arc<ChainProvider>,
    addrs: Arc<LookupAddresses>,
    cfg: Arc<AppConfig>,
) {
    tokio::spawn(async move {
        let wallet = match address.trim().parse::<alloy::primitives::Address>() {
            Ok(a) => a,
            Err(e) => {
                let _ = tx
                    .send(AppEvent::OperatorLookupFailed {
                        message: format!("{address:?} no es una dirección EVM válida: {e}"),
                    })
                    .await;
                return;
            }
        };

        let kind = crate::data::operator_tracker::classify_deployer(&provider, wallet).await;
        let list = Watchlist::load(DEFAULT_WATCHLIST_PATH).unwrap_or_default();
        let entrada = list.find(wallet);

        let mut view = crate::app::state::OperatorView {
            address: format!("{wallet:#x}"),
            kind_label: kind.label(),
            kind_needs_warning: kind.needs_warning(),
            kind_is_wallet: kind.is_wallet(),
            in_watchlist: entrada.is_some(),
            watchlist_label: entrada.and_then(|e| e.label.clone()),
            ..Default::default()
        };

        // De cuándo era el snapshot anterior. Informativo: la caché no se
        // sirve sola (decisión del 2026-09-18).
        if let Ok(db) = crate::data::db::Db::open(&cfg.indexer.db_path) {
            if let Ok(Some(info)) = db.operator_cache_info(wallet) {
                view.cached_at = Some(info.cached_at);
            }
        }

        let _ = tx
            .send(AppEvent::OperatorClassified { view: Box::new(view.clone()) })
            .await;

        let profile = match crate::data::operator_tracker::backfill_operator_history(
            &provider,
            addrs.factory,
            wallet,
            cfg.indexer.backfill_max_blocks_per_request,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                let _ = tx
                    .send(AppEvent::OperatorLookupFailed {
                        message: format!("el historial de lanzamientos falló: {e}"),
                    })
                    .await;
                return;
            }
        };

        view.launches = profile.launches.len();
        view.median_interval_secs = profile.median_interval_secs();
        view.pair_tokens = profile
            .distinct_pair_tokens()
            .into_iter()
            .map(|(a, n)| (a.to_string(), n))
            .collect();
        view.history_from_block = profile.history_from_block;
        view.history_to_block = profile.history_to_block;
        view.recent_launches = profile
            .launches
            .iter()
            .rev()
            .take(50)
            .rev()
            .map(|l| crate::app::state::OperatorLaunchRow {
                block: l.block,
                timestamp: l.timestamp,
                token: l.token.to_string(),
                pair_token: l.pair_token.to_string(),
            })
            .collect();

        // Write-through, igual que el CLI: el perfil cuesta un backfill.
        // `None` en fundings = no tocar esa tabla (aquí no se han calculado).
        if let Ok(mut db) = crate::data::db::Db::open(&cfg.indexer.db_path) {
            if let Err(e) = db.save_operator_profile(&profile, None) {
                tracing::warn!("no se pudo cachear el perfil: {e}");
            }
        }

        let _ = tx
            .send(AppEvent::OperatorHistoryResolved { view: Box::new(view) })
            .await;
    });
}

/// Financiación del operador en pantalla. Es la consulta cara (minutos), por
/// eso va detrás de una tecla y en su propia tarea.
fn spawn_fundings(
    address: String,
    tx: tokio::sync::mpsc::Sender<AppEvent>,
    provider: Arc<ChainProvider>,
    addrs: Arc<LookupAddresses>,
    cfg: Arc<AppConfig>,
) {
    use crate::data::operator_tracker::funding::{
        backfill_erc20_fundings, fill_timestamps, find_native_fundings, native_vs_erc20,
    };
    use crate::data::operator_tracker::{build_baselines, classify_fundings, summarize};

    tokio::spawn(async move {
        let fallo = |e: String| AppEvent::OperatorFundingsFailed { message: e };
        let wallet = match address.trim().parse::<alloy::primitives::Address>() {
            Ok(a) => a,
            Err(e) => {
                let _ = tx.send(fallo(format!("dirección inválida: {e}"))).await;
                return;
            }
        };
        let chunk = cfg.indexer.backfill_max_blocks_per_request;

        // El historial acota el rango y da los timestamps con los que se cruza
        // financiación → lanzamiento; se recalcula aquí para no depender de lo
        // que haya en pantalla.
        let profile = match crate::data::operator_tracker::backfill_operator_history(
            &provider, addrs.factory, wallet, chunk,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.send(fallo(format!("historial: {e}"))).await;
                return;
            }
        };
        let Some(first) = profile.launches.first() else {
            let _ = tx
                .send(fallo(
                    "sin lanzamientos: no hay ventana de financiación que medir.".to_string(),
                ))
                .await;
            return;
        };
        // La financiación que hizo posible el primer lanzamiento es anterior a él.
        let from_block = first.block.saturating_sub(200_000).max(1);
        let to_block = profile.history_to_block;

        let mut fundings =
            match backfill_erc20_fundings(&provider, wallet, from_block, to_block, chunk).await {
                Ok(f) => f,
                Err(e) => {
                    let _ = tx.send(fallo(format!("ERC-20: {e}"))).await;
                    return;
                }
            };
        let scan = match find_native_fundings(&provider, wallet, from_block, to_block).await {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(fallo(format!("nativo: {e}"))).await;
                return;
            }
        };
        let truncated = scan.truncated;
        fundings.extend(scan.fundings);
        if let Err(e) = fill_timestamps(&provider, &mut fundings).await {
            tracing::warn!("timestamps de financiación incompletos: {e}");
        }
        fundings.sort_by_key(|f| f.block);
        let (native, erc20) = native_vs_erc20(&fundings);

        let classified = match classify_fundings(&provider, &profile, fundings).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(fallo(format!("clasificación: {e}"))).await;
                return;
            }
        };
        let resumen = summarize(&classified);
        let baselines = build_baselines(&classified);

        let startup_rows: Vec<String> = classified
            .iter()
            .filter(|c| c.is_startup())
            .map(|c| {
                format!(
                    "{}  {:.6}  {}{}",
                    crate::cli::fmt_time(c.funding.timestamp),
                    c.funding.amount,
                    if c.is_high_confidence() { "alta" } else { "BAJA" },
                    if c.precedes_first_launch { "  ANTES DEL 1er LANZAMIENTO" } else { "" }
                )
            })
            .collect();

        let baseline_rows: Vec<String> = baselines
            .iter()
            .map(|b| {
                format!(
                    "baseline {} muestras: min {:.6} · mediana {:.6} · max {:.6}",
                    b.samples, b.min, b.median, b.max
                )
            })
            .collect();

        // Se relee el perfil para reconstruir la vista completa: la tarea no
        // puede leer `AppState`, que es del render loop.
        let list = Watchlist::load(DEFAULT_WATCHLIST_PATH).unwrap_or_default();
        let entrada = list.find(wallet);
        let kind = profile.kind;
        let view = crate::app::state::OperatorView {
            address: format!("{wallet:#x}"),
            kind_label: kind.label(),
            kind_needs_warning: kind.needs_warning(),
            kind_is_wallet: kind.is_wallet(),
            in_watchlist: entrada.is_some(),
            watchlist_label: entrada.and_then(|e| e.label.clone()),
            launches: profile.launches.len(),
            median_interval_secs: profile.median_interval_secs(),
            pair_tokens: profile
                .distinct_pair_tokens()
                .into_iter()
                .map(|(a, n)| (a.to_string(), n))
                .collect(),
            history_from_block: profile.history_from_block,
            history_to_block: profile.history_to_block,
            recent_launches: profile
                .launches
                .iter()
                .rev()
                .take(50)
                .rev()
                .map(|l| crate::app::state::OperatorLaunchRow {
                    block: l.block,
                    timestamp: l.timestamp,
                    token: l.token.to_string(),
                    pair_token: l.pair_token.to_string(),
                })
                .collect(),
            cached_at: None,
            fundings_native: native,
            fundings_erc20: erc20,
            startup_high: resumen.startup_high,
            startup_low: resumen.startup_low,
            income: resumen.native_internal + resumen.sender_is_contract + resumen.self_initiated,
            not_evaluated: resumen.not_evaluated,
            startup_rows,
            baseline_rows,
            funding_truncated: truncated,
        };

        if let Ok(mut db) = crate::data::db::Db::open(&cfg.indexer.db_path) {
            if let Err(e) = db.save_operator_profile(&profile, Some(&classified)) {
                tracing::warn!("no se pudo cachear la financiación: {e}");
            }
        }

        let _ = tx
            .send(AppEvent::OperatorFundingsResolved { view: Box::new(view) })
            .await;
    });
}
