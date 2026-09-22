//! Panel de la pestaña **Operador**: perfil de una wallet como deployer de
//! launchpad.
//!
//! Se llega aquí desde el mismo campo del buscador: si la dirección pegada no
//! es un launch de Pons V2, `ui::run` reintenta la consulta por esta vía. No
//! hay campo de entrada propio a propósito — dos campos que aceptan lo mismo
//! solo obligan a elegir antes de saber qué es la dirección.
//!
//! Como el resto de paneles nuevos, **respeta el área que le da el layout**
//! (las heredadas no lo hacen: deuda nº11).

use crate::app::state::{FundingStatus, OperatorStatus, OperatorView};
use crate::app::App;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

const DIM: Color = Color::Rgb(120, 130, 140);
const WARN: Color = Color::Rgb(239, 83, 80);
const OK: Color = Color::Rgb(38, 166, 154);

pub fn draw_in(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9), // ficha
            Constraint::Min(6),    // lanzamientos + financiación
            Constraint::Length(1), // ayuda de teclas
        ])
        .split(area);

    draw_card(frame, app, chunks[0]);
    draw_body(frame, app, chunks[1]);
    draw_help(frame, app, chunks[2]);
}

fn draw_card(frame: &mut Frame, app: &App, area: Rect) {
    let s = &app.state;
    let block = Block::default().borders(Borders::ALL).title(" operador ");

    let lines: Vec<Line> = match (&s.operator, &s.operator_status) {
        (_, OperatorStatus::Idle) => vec![
            Line::from(Span::styled(
                "pega una dirección en el Buscador y pulsa enter.",
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled(
                "si no es un token de Pons V2, se consulta aquí como wallet de operador.",
                Style::default().fg(DIM),
            )),
        ],
        (_, OperatorStatus::Failed(msg)) => vec![Line::from(Span::styled(
            msg.clone(),
            Style::default().fg(WARN),
        ))],
        (None, st) => vec![Line::from(Span::styled(
            estado_texto(st),
            Style::default().fg(Color::Yellow),
        ))],
        (Some(v), st) => ficha(v, st),
    };

    frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn estado_texto(st: &OperatorStatus) -> String {
    match st {
        OperatorStatus::Classifying => "comprobando si es wallet o contrato...".to_string(),
        OperatorStatus::LoadingHistory => {
            "trayendo el historial de lanzamientos (filtrado por el nodo)...".to_string()
        }
        OperatorStatus::Done => String::new(),
        OperatorStatus::Idle => String::new(),
        OperatorStatus::Failed(m) => m.clone(),
    }
}

fn ficha<'a>(v: &'a OperatorView, st: &OperatorStatus) -> Vec<Line<'a>> {
    let mut out = vec![Line::from(vec![
        Span::styled(v.address.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            v.kind_label.clone(),
            Style::default().fg(if v.kind_needs_warning { WARN } else { OK }),
        ),
    ])];

    // El aviso de relay es la razón de ser de `classify_deployer`: el deployer
    // más prolífico de la chain es Multicall3, un relay por el que pasan
    // lanzamientos de terceros. Se muestra el perfil igual, pero se dice.
    if v.kind_needs_warning {
        out.push(Line::from(Span::styled(
            "!! no confirmada como wallet: sus lanzamientos pueden ser de terceros (caso Multicall3)",
            Style::default().fg(WARN),
        )));
    }

    out.push(Line::from(vec![
        Span::styled("watchlist  ", Style::default().fg(DIM)),
        if v.in_watchlist {
            Span::styled(
                match &v.watchlist_label {
                    Some(l) => format!("sí — {l}"),
                    None => "sí".to_string(),
                },
                Style::default().fg(OK),
            )
        } else {
            Span::styled("no  (w para añadir)", Style::default().fg(DIM))
        },
    ]));

    out.push(Line::from(vec![
        Span::styled("lanzamientos  ", Style::default().fg(DIM)),
        Span::styled(v.launches.to_string(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("   cadencia  ", Style::default().fg(DIM)),
        Span::raw(match v.median_interval_secs {
            Some(s) => crate::cli::fmt_duration(s),
            None => "n/d".to_string(),
        }),
        Span::styled("   pairTokens  ", Style::default().fg(DIM)),
        Span::raw(v.pair_tokens.len().to_string()),
    ]));

    out.push(Line::from(Span::styled(
        format!("rango buscado: bloques {} → {}", v.history_from_block, v.history_to_block),
        Style::default().fg(DIM),
    )));

    if let Some(ts) = v.cached_at {
        out.push(Line::from(Span::styled(
            format!(
                "había un snapshot cacheado del {} (no se sirve solo: lo de arriba viene de la chain)",
                crate::cli::fmt_time(ts)
            ),
            Style::default().fg(DIM),
        )));
    }

    if st.is_loading() {
        out.push(Line::from(Span::styled(
            estado_texto(st),
            Style::default().fg(Color::Yellow),
        )));
    }
    out
}

fn draw_body(frame: &mut Frame, app: &App, area: Rect) {
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    draw_launches(frame, app, halves[0]);
    draw_funding(frame, app, halves[1]);
}

fn draw_launches(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" lanzamientos (más reciente abajo) ");
    let Some(v) = &app.state.operator else {
        frame.render_widget(block, area);
        return;
    };

    let mut lines = vec![Line::from(Span::styled(
        format!("{:<10} {:<17} {:<12} {}", "bloque", "fecha (UTC)", "token", "par"),
        Style::default().fg(DIM),
    ))];

    // Se pinta solo lo que cabe: una wallet puede tener 1.200 lanzamientos y
    // no tiene sentido construir 1.200 líneas para enseñar 20.
    let cabe = (area.height as usize).saturating_sub(3);
    let desde = v.recent_launches.len().saturating_sub(cabe);
    for l in v.recent_launches.iter().skip(desde) {
        lines.push(Line::from(format!(
            "{:<10} {:<17} {:<12} {}",
            l.block,
            crate::cli::fmt_time(l.timestamp),
            corto(&l.token),
            corto(&l.pair_token),
        )));
    }
    if v.recent_launches.is_empty() {
        lines.push(Line::from(Span::styled(
            "esta wallet no ha lanzado ningún token con la factory V2.",
            Style::default().fg(DIM),
        )));
    }

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_funding(frame: &mut Frame, app: &App, area: Rect) {
    let s = &app.state;
    let block = Block::default().borders(Borders::ALL).title(" financiación ");

    let lines: Vec<Line> = match (&s.funding_status, &s.operator) {
        (FundingStatus::NotRequested, _) => vec![
            Line::from(Span::styled("pulsa `f` para medirla.", Style::default().fg(DIM))),
            Line::from(Span::styled("", Style::default())),
            Line::from(Span::styled(
                "no se carga sola porque es la parte cara: el Transfer de ERC-20 va",
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled(
                "sin filtro de address sobre toda la chain y el nativo por bisección",
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled("de saldo. Son minutos, no segundos.", Style::default().fg(DIM))),
        ],
        (FundingStatus::Loading, _) => vec![
            Line::from(Span::styled(
                "midiendo financiación... (minutos)",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(Span::styled(
                "el resto de la TUI sigue respondiendo.",
                Style::default().fg(DIM),
            )),
        ],
        (FundingStatus::Failed(m), _) => {
            vec![Line::from(Span::styled(m.clone(), Style::default().fg(WARN)))]
        }
        (FundingStatus::Done, Some(v)) => resumen_financiacion(v),
        (FundingStatus::Done, None) => vec![],
    };

    frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn resumen_financiacion(v: &OperatorView) -> Vec<Line<'_>> {
    let total = v.fundings_native + v.fundings_erc20;
    let mut out = vec![Line::from(vec![
        Span::styled("entradas  ", Style::default().fg(DIM)),
        Span::raw(format!(
            "{total}   nativo {}  ·  ERC-20 {}",
            v.fundings_native, v.fundings_erc20
        )),
    ])];

    if v.funding_truncated {
        out.push(Line::from(Span::styled(
            "!! el escaneo nativo llegó a su tope: el recuento es un SUELO",
            Style::default().fg(WARN),
        )));
    }

    out.push(Line::from(Span::styled(
        format!(
            "arranque: {} alta · {} baja    ingreso: {}    sin evaluar: {}",
            v.startup_high, v.startup_low, v.income, v.not_evaluated
        ),
        Style::default().fg(DIM),
    )));

    if v.startup_rows.is_empty() {
        out.push(Line::from(Span::styled(
            "ninguna entrada pasa el criterio de financiación de arranque.",
            Style::default().fg(DIM),
        )));
    } else {
        out.push(Line::from(""));
        for r in &v.startup_rows {
            out.push(Line::from(r.clone()));
        }
    }

    out.push(Line::from(""));
    if v.baseline_rows.is_empty() {
        // Con el criterio aprobado este es el camino normal, no un fallo: la
        // muestra real es diminuta (1 financiación en 1.277 lanzamientos).
        out.push(Line::from(Span::styled(
            "baseline: sin muestra suficiente (es lo esperable, no un fallo)",
            Style::default().fg(DIM),
        )));
    } else {
        for r in &v.baseline_rows {
            out.push(Line::from(r.clone()));
        }
    }
    out
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let en_watchlist = app.state.operator.as_ref().map(|o| o.in_watchlist).unwrap_or(false);
    let txt = format!(
        "f financiación  ·  {}  ·  r recargar perfil  ·  tab cambia panel",
        if en_watchlist { "w quitar de la watchlist" } else { "w añadir a la watchlist" }
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(txt, Style::default().fg(DIM)))),
        area,
    );
}

/// Dirección abreviada, que a 44 caracteres no cabe nada más en la fila.
fn corto(addr: &str) -> String {
    if addr.len() > 12 {
        format!("{}…{}", &addr[..6], &addr[addr.len() - 4..])
    } else {
        addr.to_string()
    }
}
