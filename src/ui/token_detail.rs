//! Panel principal de análisis: gráfico de velas del token seleccionado con
//! los indicadores superpuestos/apilados (ver conversación de diseño:
//! mismo renderer de velas que hyperT, portando el framework de
//! whales_rsi_adx_dmi.pine — BB+RSI/ADX/DMI + histograma de señal de
//! whale, adaptado a acumulación/flujo de un memecoin en vez de OI/funding
//! de perps).
//!
//! Placeholder por ahora: el renderer de velas real se portará desde hyperT
//! cuando se llegue a esta pieza, no se reimplementa desde cero aquí.

use crate::app::App;
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App) {
    let title = app
        .state
        .selected_token
        .as_ref()
        .map(|t| format!("Token: {} ({:?})", t.address, t.phase))
        .unwrap_or_else(|| "Ningún token seleccionado".to_string());

    frame.render_widget(
        ratatui::widgets::Paragraph::new("Gráfico + indicadores — pendiente de portar renderer de hyperT")
            .block(ratatui::widgets::Block::bordered().title(title)),
        frame.area(),
    );

    // TODO: panel de velas (reutilizar renderer de hyperT)
    // TODO: panel apilado de indicadores (RSI/ADX/DMI + histograma whale)
    // TODO: acción rápida comprar/vender con preflight visible antes de confirmar
}
