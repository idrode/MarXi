//! Panel del buscador por-token: campo de dirección, ficha del token y
//! gráfico de velas. Es la pantalla de arranque del programa.
//!
//! Todo lo que pinta sale de `AppState`; no hace ninguna llamada de red. La
//! consulta la lanza `ui::run` en una tarea aparte y vuelve por el canal de
//! `AppEvent`.

use crate::app::state::{SearchStatus, TokenView};
use crate::app::App;
use crate::data::candles::Candle;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;

/// Verde y rojo de velas. Se usan también en la ficha para el signo de la
/// variación, para que el color signifique lo mismo en toda la pantalla.
const UP: Color = Color::Rgb(38, 166, 154);
const DOWN: Color = Color::Rgb(239, 83, 80);
const DIM: Color = Color::Rgb(120, 130, 140);

/// Dibuja el panel dentro del área que le da el layout general.
pub fn draw_in(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // campo de búsqueda
            Constraint::Length(9), // ficha del token
            Constraint::Min(8),    // gráfico
        ])
        .split(area);

    draw_input(frame, app, chunks[0]);
    draw_card(frame, app, chunks[1]);
    draw_chart(frame, app, chunks[2]);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let s = &app.state;
    let (border, hint) = match &s.search_status {
        SearchStatus::LoadingState => (Color::Yellow, " consultando estado on-chain… "),
        SearchStatus::LoadingHistory => (Color::Yellow, " trayendo histórico (puede tardar) … "),
        SearchStatus::Failed(_) => (DOWN, " enter para reintentar "),
        _ => (Color::DarkGray, " pega una dirección y pulsa enter "),
    };

    let cursor = if s.search_focused { "▏" } else { "" };
    let text = Line::from(vec![
        Span::styled("token  ", Style::default().fg(DIM)),
        Span::raw(&s.search_input),
        Span::styled(cursor, Style::default().fg(Color::White)),
    ]);

    frame.render_widget(
        Paragraph::new(text).block(
            Block::bordered()
                .border_style(Style::default().fg(border))
                .title(" buscador por-token ")
                .title_bottom(hint),
        ),
        area,
    );
}

fn draw_card(frame: &mut Frame, app: &App, area: Rect) {
    let s = &app.state;

    if let SearchStatus::Failed(msg) = &s.search_status {
        frame.render_widget(
            Paragraph::new(msg.as_str())
                .style(Style::default().fg(DOWN))
                .wrap(Wrap { trim: true })
                .block(Block::bordered().title(" error ")),
            area,
        );
        return;
    }

    let Some(v) = &s.selected_token else {
        frame.render_widget(
            Paragraph::new(
                "Pega la dirección de un token de Pons V2.\n\
                 Se consulta al momento: no hace falta servidor ni índice previo.",
            )
            .style(Style::default().fg(DIM))
            .block(Block::bordered().title(" sin token ")),
            area,
        );
        return;
    };

    let pair = v.pair_symbol.clone().unwrap_or_else(|| "par".into());
    let mut rows: Vec<Line> = Vec::new();

    rows.push(Line::from(vec![
        Span::styled(
            v.symbol.clone().unwrap_or_else(|| "¿?".into()),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(v.address.clone(), Style::default().fg(DIM)),
    ]));
    rows.push(field("fase", v.phase.label().to_string()));
    rows.push(field(
        "precio",
        match v.price_in_pair {
            Some(p) => format!("{} {pair}", crate::cli::fmt_price(p)),
            None => "sin precio en esta fase".into(),
        },
    ));
    rows.push(field(
        "market cap",
        match v.market_cap_in_pair {
            Some(m) => format!("{} {pair}", crate::cli::fmt_amount(m)),
            None => "n/d".into(),
        },
    ));

    // La variación lleva color: es el dato que se mira de un vistazo.
    let change = match v.change_since_first_trade_pct {
        Some(c) => Span::styled(
            format!("{c:+.2} % desde el primer trade"),
            Style::default().fg(if c >= 0.0 { UP } else { DOWN }),
        ),
        None => Span::styled("n/d", Style::default().fg(DIM)),
    };
    rows.push(Line::from(vec![label("variación"), change]));

    rows.push(field(
        "holders",
        v.holders.map(|h| h.to_string()).unwrap_or_else(|| "n/d".into()),
    ));

    if let Some(g) = v.graduation_progress {
        rows.push(Line::from(vec![
            label("graduación"),
            Span::raw(progress_bar(g, 24)),
            Span::raw(format!(" {:.1} %", g * 100.0)),
        ]));
    } else if let Some(pool) = &v.pool_id {
        rows.push(field("poolId", short_hash(pool)));
    }

    frame.render_widget(
        Paragraph::new(rows).block(Block::bordered().title(title_for(v, &s.search_status))),
        area,
    );
}

fn title_for(v: &TokenView, status: &SearchStatus) -> String {
    let n = v.trades_indexed.unwrap_or(0);
    match status {
        SearchStatus::LoadingHistory => " token (trayendo histórico…) ".into(),
        _ if n > 0 => format!(" token · {n} trades indexados "),
        _ => " token ".into(),
    }
}

fn label(name: &str) -> Span<'static> {
    Span::styled(format!("{name:<12}"), Style::default().fg(DIM))
}

fn field(name: &str, value: String) -> Line<'static> {
    Line::from(vec![label(name), Span::raw(value)])
}

fn short_hash(h: &str) -> String {
    if h.len() > 18 {
        format!("{}…{}", &h[..10], &h[h.len() - 6..])
    } else {
        h.to_string()
    }
}

fn progress_bar(frac: f64, width: usize) -> String {
    let filled = ((frac.clamp(0.0, 1.0)) * width as f64).round() as usize;
    format!("[{}{}]", "█".repeat(filled), "·".repeat(width - filled))
}

/// Gráfico de velas en modo texto.
///
/// Cada columna es una vela y se dibuja con medios bloques: cuerpo lleno
/// entre apertura y cierre, mecha fina entre máximo y mínimo. Es el renderer
/// mínimo para que el buscador sea usable; el renderer rico con indicadores
/// se porta desde hyperT en la Fase 4, como dice CLAUDE.md.
fn draw_chart(frame: &mut Frame, app: &App, area: Rect) {
    let candles = &app.state.candles;
    let block = Block::bordered().title(format!(" velas ({}) ", candles.len()));

    if candles.is_empty() {
        let msg = if app.state.search_status.is_loading() {
            "agregando velas…"
        } else {
            "sin velas todavía"
        };
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(DIM)).block(block),
            area,
        );
        return;
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 3 || inner.width < 10 {
        return;
    }

    // Solo caben las últimas N velas, una por columna. Se reserva un margen
    // izquierdo para la escala de precios.
    let gutter = 14u16.min(inner.width / 3);
    let plot_width = inner.width.saturating_sub(gutter) as usize;
    let shown: Vec<&Candle> = candles.iter().rev().take(plot_width).rev().collect();
    if shown.is_empty() {
        return;
    }

    let high = shown.iter().map(|c| c.high).fold(f64::MIN, f64::max);
    let low = shown.iter().map(|c| c.low).fold(f64::MAX, f64::min);
    let span = (high - low).max(f64::EPSILON);
    let rows = inner.height as usize;

    // Cada fila de texto cubre una banda de precio; una vela ocupa la banda
    // que va de su apertura a su cierre.
    let y_of = |price: f64| -> f64 { (high - price) / span * (rows as f64 - 1.0) };

    let mut lines: Vec<Line> = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut spans = vec![Span::styled(
            format!("{:>width$} ", scale_label(high, low, row, rows), width = gutter as usize - 1),
            Style::default().fg(DIM),
        )];
        for c in &shown {
            let bull = c.close >= c.open;
            let (body_top, body_bottom) = if bull { (c.close, c.open) } else { (c.open, c.close) };
            let (t, b) = (y_of(body_top), y_of(body_bottom));
            let (wt, wb) = (y_of(c.high), y_of(c.low));
            let r = row as f64;

            // Redondear a la celda: el cuerpo se pinta si la fila cae dentro
            // del rango apertura-cierre, la mecha si cae en máximo-mínimo.
            let in_body = r >= t.floor() && r <= b.ceil();
            let in_wick = r >= wt.floor() && r <= wb.ceil();
            let ch = if in_body {
                "█"
            } else if in_wick {
                "│"
            } else {
                " "
            };
            spans.push(Span::styled(
                ch,
                Style::default().fg(if bull { UP } else { DOWN }),
            ));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn scale_label(high: f64, low: f64, row: usize, rows: usize) -> String {
    // Solo se etiquetan la fila de arriba, la de abajo y la del medio: más
    // etiquetas hacen ilegible una escala de precios con doce decimales.
    let frac = row as f64 / (rows as f64 - 1.0).max(1.0);
    if row == 0 || row == rows - 1 || row == rows / 2 {
        crate::cli::fmt_price(high - (high - low) * frac)
    } else {
        String::new()
    }
}
