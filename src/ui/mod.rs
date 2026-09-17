//! Loop de render con ratatui. Puramente síncrono: lee `AppState`, dibuja,
//! espera input de teclado o el siguiente `AppEvent`. Ninguna llamada de red
//! ni de disco ocurre aquí — eso vive en tareas tokio spawneadas que
//! comunican por el canal `mpsc`.
//!
//! Invariante del proyecto que se respeta aquí: **solo `App::apply_event`
//! muta `AppState`**. Por eso hasta las teclas se convierten en `AppEvent`
//! antes de tocar nada, en vez de escribir en el estado desde el loop.

pub mod dashboard;
pub mod positions;
pub mod search;
pub mod settings;
pub mod sniper;
pub mod token_detail;

use crate::app::{state::Tab, App, AppEvent};
use crate::chain::ChainProvider;
use crate::config::AppConfig;
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

    let mut terminal = init_terminal()?;
    let result = event_loop(&mut terminal, &mut app, &mut key_rx, &provider, &addrs, &cfg).await;
    restore_terminal()?;
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
) -> anyhow::Result<()> {
    let mut blink = std::time::Instant::now();

    loop {
        terminal.draw(|f| draw(f, app))?;
        if app.state.should_quit {
            return Ok(());
        }

        tokio::select! {
            Some(key) = key_rx.recv() => {
                for ev in map_key(key, app) {
                    // Pulsar enter en el buscador es lo único que dispara
                    // trabajo de red: se lanza aparte y el loop sigue vivo.
                    if let AppEvent::TokenLookupStarted { address } = &ev {
                        spawn_lookup(
                            address.clone(),
                            app.event_tx.clone(),
                            provider.clone(),
                            addrs.clone(),
                            cfg.clone(),
                        );
                    }
                    app.apply_event(ev);
                }
            }
            Some(ev) = app.event_rx.recv() => {
                app.apply_event(ev);
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
                let _ = tx.send(AppEvent::TokenLookupFailed { message: format!("{e}") }).await;
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
        spans.push(Span::styled(
            format!(" {} ", t.title()),
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
    let mut parts = vec!["tab/shift-tab cambia panel".to_string(), "esc sale".to_string()];
    if let Some(b) = app.state.last_block_seen {
        parts.push(format!("bloque {b}"));
    }
    if let Some(e) = &app.state.last_background_error {
        parts.push(format!("error: {e}"));
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            parts.join("  ·  "),
            Style::default().fg(Color::DarkGray),
        )))
        .block(Block::default()),
        area,
    );
}
