//! Feed en vivo de TokenLaunched / PoolGraduated de los launchpads
//! habilitados, con acceso rápido a "ver detalle" / "comprar ya" del token
//! seleccionado en la lista.

use crate::app::App;
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App) {
    let items: Vec<String> = app
        .state
        .recent_launches
        .iter()
        .rev()
        .take(20)
        .map(|(lp, addr)| format!("[{lp}] {addr}"))
        .collect();

    let list = ratatui::widgets::List::new(items)
        .block(ratatui::widgets::Block::bordered().title("Sniper — lanzamientos recientes"));

    frame.render_widget(list, frame.area());

    // TODO: distinguir visualmente fase curve vs graduado
    // TODO: progreso de graduación (curve_engine::graduation_progress) por token en lista
}
