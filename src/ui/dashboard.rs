//! Vista general: balance, últimos bloques vistos, resumen de actividad
//! reciente de los launchpads seguidos.

use crate::app::App;
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App) {
    let block_info = app
        .state
        .last_block_seen
        .map(|n| format!("Último bloque: {n}"))
        .unwrap_or_else(|| "Conectando...".to_string());

    frame.render_widget(
        ratatui::widgets::Paragraph::new(block_info)
            .block(ratatui::widgets::Block::bordered().title("Dashboard")),
        frame.area(),
    );

    // TODO: balance ETH + tokens principales (pedido explícito del MVP)
    // TODO: resumen de recent_launches / recent_graduations
}
