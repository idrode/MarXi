//! Lista de posiciones abiertas con PnL en vivo y configuración de TP/SL.

use crate::app::App;
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App) {
    let rows: Vec<String> = app
        .state
        .positions
        .iter()
        .map(|p| format!("{} — entrada: {} — cantidad: {}", p.token_address, p.entry_price, p.amount))
        .collect();

    let list = ratatui::widgets::List::new(rows)
        .block(ratatui::widgets::Block::bordered().title("Posiciones"));

    frame.render_widget(list, frame.area());

    // TODO: PnL en vivo (necesita precio actual del token vía data::candles)
    // TODO: edición inline de take_profit / stop_loss
}
