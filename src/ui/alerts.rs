//! Panel de la pestaña **Alertas**: la señal 2 en vivo.
//!
//! Lo que se ve aquí es lo que ha pasado **mientras el programa ha estado
//! abierto**. El registro completo, con su resultado, vive en la base
//! (`cargo run -- alert list`): esta lista está acotada a propósito.
//!
//! El `id` de cada alerta se enseña porque es la pieza del ciclo de prueba y
//! error: es lo que se pasa a `cargo run -- alert <id> <outcome> [nota]`.

use crate::app::state::{LiveAlert, WatchState};
use crate::app::App;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

const DIM: Color = Color::Rgb(120, 130, 140);
const WARN: Color = Color::Rgb(239, 83, 80);
const OK: Color = Color::Rgb(38, 166, 154);
const HOT: Color = Color::Rgb(255, 193, 7);

/// Cuántos segundos se resalta la alerta recién llegada.
const RESALTE_SECS: u64 = 30;

pub fn draw_in(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(4), Constraint::Length(1)])
        .split(area);

    draw_watch_state(frame, app, chunks[0]);
    draw_list(frame, app, chunks[1]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "v arranca/para la vigilancia  ·  R recarga watchlist.toml  ·  cierra una alerta con: cargo run -- alert <id> <outcome>",
            Style::default().fg(DIM),
        ))),
        chunks[2],
    );
}

fn draw_watch_state(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" vigilancia (señal 2) ");
    let lines = match &app.state.watch {
        WatchState::Idle => vec![Line::from(Span::styled(
            "sin arrancar.",
            Style::default().fg(DIM),
        ))],
        WatchState::NoWatchlist => vec![
            Line::from(Span::styled(
                "watchlist vacía: no hay nada que vigilar.",
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled(
                "añade wallets con `cargo run -- watch <addr>` o con `w` en la pestaña Operador, y pulsa R.",
                Style::default().fg(DIM),
            )),
        ],
        WatchState::Starting => vec![Line::from(Span::styled(
            "arrancando...",
            Style::default().fg(Color::Yellow),
        ))],
        WatchState::Running { watched, transport, from_block } => vec![
            Line::from(vec![
                Span::styled("activa  ", Style::default().fg(OK).add_modifier(Modifier::BOLD)),
                Span::raw(format!("{watched} wallet(s)")),
                Span::styled("   vía  ", Style::default().fg(DIM)),
                Span::raw(transport.clone()),
                Span::styled("   desde el bloque  ", Style::default().fg(DIM)),
                Span::raw(from_block.to_string()),
            ]),
            Line::from(Span::styled(
                "solo se ve lo que ocurra a partir de ahí; el historial previo es `cargo run -- operator`.",
                Style::default().fg(DIM),
            )),
        ],
        WatchState::Failed(m) => vec![
            Line::from(Span::styled("NO está vigilando: ", Style::default().fg(WARN))),
            Line::from(Span::styled(m.clone(), Style::default().fg(WARN))),
        ],
    };
    frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect) {
    let s = &app.state;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" alertas de esta sesión ({}) ", s.alerts.len()));

    if s.alerts.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "ninguna todavía.",
                Style::default().fg(DIM),
            )))
            .block(block),
            area,
        );
        return;
    }

    let ahora = now_secs();
    let mut lines = Vec::new();
    // Dos filas por alerta, la más reciente arriba: es la que interesa.
    let cabe = ((area.height as usize).saturating_sub(2)) / 2;
    for a in s.alerts.iter().rev().take(cabe.max(1)) {
        let reciente = ahora.saturating_sub(a.received_at) <= RESALTE_SECS;
        lines.push(fila_principal(a, reciente));
        lines.push(fila_detalle(a));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn fila_principal(a: &LiveAlert, reciente: bool) -> Line<'static> {
    let estilo = if reciente {
        Style::default().fg(HOT).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let quien = match &a.label {
        Some(l) => format!("{l} ({})", corto(&a.deployer)),
        None => corto(&a.deployer),
    };
    Line::from(vec![
        Span::styled(
            match a.alert_id {
                Some(id) => format!("#{id:<5}"),
                // Sin id: la alerta llegó pero no se pudo escribir en disco.
                None => "#  —  ".to_string(),
            },
            Style::default().fg(DIM),
        ),
        Span::styled(crate::cli::fmt_time(a.received_at), Style::default().fg(DIM)),
        Span::raw("  "),
        Span::styled(quien, estilo),
        Span::raw("  lanzó  "),
        Span::styled(corto(&a.token), estilo),
    ])
}

fn fila_detalle(a: &LiveAlert) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(
            "       par {}  ·  umbral {}  ·  bloque {}  ·  tx {}",
            corto(&a.pair_token),
            a.graduation_threshold,
            a.block,
            corto(&a.tx_hash)
        ),
        Style::default().fg(DIM),
    )];
    if a.alert_id.is_none() {
        spans.push(Span::styled(
            "   [no registrada en la base]",
            Style::default().fg(WARN),
        ));
    }
    Line::from(spans)
}

fn corto(s: &str) -> String {
    if s.len() > 14 {
        format!("{}…{}", &s[..8], &s[s.len() - 4..])
    } else {
        s.to_string()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
