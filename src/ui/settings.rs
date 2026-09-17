//! Ajustes en vivo: slippage por defecto, RPC en uso, launchpads
//! habilitados. Cambios aquí deberían persistir de vuelta a config.toml
//! (pendiente de decidir si autoguardado o explícito con confirmación).

use crate::app::App;
use ratatui::Frame;

pub fn draw(frame: &mut Frame, _app: &App) {
    frame.render_widget(
        ratatui::widgets::Paragraph::new("Settings — pendiente de implementar")
            .block(ratatui::widgets::Block::bordered().title("Settings")),
        frame.area(),
    );
}
