//! Subcomandos de texto. Son la vía de validación de la capa de datos: si
//! `cargo run -- token <address>` imprime cifras correctas, la TUI solo tiene
//! que pintarlas, y un fallo se puede atribuir a los datos o al render sin
//! ambigüedad.
//!
//! No sustituyen a la TUI: `ui::run` es la interfaz real del proyecto.

use crate::app::state::{TokenPhase, TokenView};
use crate::chain::ChainProvider;
use crate::config::AppConfig;
use crate::data::backfill::{self, TokenHistory};
use crate::data::token_lookup::{self, LookupAddresses};
use alloy::primitives::Address;

/// Ventana de vela por defecto. Una hora es lo que hace legible un token con
/// meses de historia sin generar miles de velas.
pub const DEFAULT_CANDLE_SECONDS: u64 = 3600;

/// `cargo run -- token <address>`
pub async fn run_token_cli(cfg: &AppConfig, address: &str, candle_seconds: u64) -> anyhow::Result<()> {
    let token: Address = address
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("{address:?} no es una dirección EVM válida: {e}"))?;

    let provider = ChainProvider::connect(&cfg.chain).await?;
    let addrs = LookupAddresses::from_config(cfg)?;

    println!("consultando {token} en chain {} ...", provider.chain_id);
    let mut view = token_lookup::lookup(&provider, &addrs, token).await?;
    print_state(&view);

    println!("\ntrayendo histórico de eventos (puede tardar: el RPC público\n\
              se trocea y reintenta ante 429) ...");
    let started = std::time::Instant::now();
    let history = backfill::backfill(
        &provider,
        addrs.pool_manager,
        addrs.factory,
        token,
        &view,
        cfg.indexer.backfill_max_blocks_per_request,
        candle_seconds,
        true,
    )
    .await?;

    view.holders = history.holders;
    view.trades_indexed = Some(history.trades.len());
    view.change_since_first_trade_pct = history.change_pct(view.price_in_pair);
    view.first_event_block = history.launch_block;

    print_history(&view, &history, candle_seconds, started.elapsed());
    Ok(())
}

fn print_history(v: &TokenView, h: &TokenHistory, candle_seconds: u64, elapsed: std::time::Duration) {
    let pair = v.pair_symbol.clone().unwrap_or_else(|| "par".into());
    println!("\nhistórico  ({:.1} s)", elapsed.as_secs_f64());
    if let Some(b) = h.launch_block {
        println!("  {:<12} {b}", "lanzamiento");
    }
    if let Some(b) = h.graduation_block {
        println!("  {:<12} {b}", "graduación");
    }
    println!("  {:<12} {}", "trades", h.trades.len());
    println!("  {:<12} {}", "transfers", h.transfer_events);
    if h.blocks_without_timestamp > 0 {
        println!("  {:<12} {} bloques sin timestamp: el gráfico está incompleto", "AVISO", h.blocks_without_timestamp);
    }
    match h.holders {
        Some(n) => println!("  {:<12} {n}", "holders"),
        None => println!("  {:<12} n/d", "holders"),
    }
    if let Some(p) = h.first_price() {
        println!("  {:<12} {} {pair}", "primer px", fmt_price(p));
    }
    if let Some(p) = h.last_price() {
        println!("  {:<12} {} {pair}", "último px", fmt_price(p));
    }
    match v.change_since_first_trade_pct {
        Some(c) => println!("  {:<12} {c:+.2} % desde el primer trade", "variación"),
        None => println!("  {:<12} n/d", "variación"),
    }

    println!("\nvelas de {candle_seconds} s: {}", h.candles.len());
    if !h.candles.is_empty() {
        println!(
            "  {:<20} {:>16} {:>16} {:>16} {:>16} {:>14}",
            "apertura (UTC)", "open", "high", "low", "close", "volumen"
        );
        for c in h.candles.iter().rev().take(5).rev() {
            println!(
                "  {:<20} {:>16} {:>16} {:>16} {:>16} {:>14}",
                fmt_time(c.open_time),
                fmt_price(c.open),
                fmt_price(c.high),
                fmt_price(c.low),
                fmt_price(c.close),
                fmt_amount(c.volume)
            );
        }
    }
}

/// Timestamp unix a texto legible sin dependencias extra de fecha.
pub fn fmt_time(ts: u64) -> String {
    let days = ts / 86400;
    let rem = ts % 86400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, (rem % 3600) / 60)
}

/// Algoritmo de Howard Hinnant para convertir días desde epoch a fecha civil.
/// Evita arrastrar `chrono` solo para imprimir una columna.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn print_state(v: &TokenView) {
    let sym = v.symbol.clone().unwrap_or_else(|| "¿?".into());
    let pair = v.pair_symbol.clone().unwrap_or_else(|| "¿par?".into());
    println!("\n{sym}  {}", v.address);
    println!("  {:<12} {}", "fase", v.phase.label());
    println!(
        "  {:<12} {pair} ({}, {} dec)",
        "par",
        v.pair_address.as_deref().unwrap_or("?"),
        v.pair_decimals.map(|d| d.to_string()).unwrap_or_else(|| "?".into())
    );
    if let Some(c) = &v.curve_address {
        println!("  {:<12} {c}", "curva");
    }
    if let Some(p) = &v.pool_id {
        println!("  {:<12} {p}", "poolId");
    }
    match v.price_in_pair {
        Some(p) => println!("  {:<12} {} {pair}", "precio", fmt_price(p)),
        None => println!("  {:<12} sin precio disponible en esta fase", "precio"),
    }
    if let Some(m) = v.market_cap_in_pair {
        println!("  {:<12} {} {pair}", "market cap", fmt_amount(m));
    }
    if let Some(s) = v.total_supply {
        println!("  {:<12} {} {sym}", "supply", fmt_amount(s));
    }
    if let Some(l) = v.pool_liquidity {
        println!("  {:<12} {l}", "liquidez");
    }
    if let Some(g) = v.graduation_progress {
        println!("  {:<12} {:.2} % hacia la graduación", "progreso", g * 100.0);
    }
    if v.phase == TokenPhase::Swept || v.phase == TokenPhase::Rescued {
        println!("  (fase transitoria o fallida: ni curva operativa ni pool V4)");
    }
}

/// Precios de memecoin son diminutos (del orden de 1e-9): notación fija con
/// muchos decimales, no científica, que es lo que se quiere leer de un vistazo.
pub fn fmt_price(p: f64) -> String {
    if p == 0.0 {
        return "0".into();
    }
    if p.abs() < 1e-12 {
        format!("{p:.3e}")
    } else {
        format!("{p:.12}")
    }
}

pub fn fmt_amount(a: f64) -> String {
    if a.abs() >= 1_000_000.0 {
        format!("{:.3e}", a)
    } else {
        format!("{a:.6}")
    }
}

/// `cargo run -- preview <address>`: renderiza el panel del buscador con
/// datos reales usando el backend de pruebas de ratatui y vuelca el resultado
/// como texto.
///
/// Existe porque una TUI no se puede inspeccionar desde un script ni desde un
/// log: esto permite comprobar el layout y las cifras exactas que vería el
/// usuario, sin abrir una terminal interactiva.
pub async fn run_preview_cli(
    cfg: &AppConfig,
    address: &str,
    width: u16,
    height: u16,
) -> anyhow::Result<()> {
    use crate::app::state::SearchStatus;
    use crate::app::App;

    let token: Address = address
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("{address:?} no es una dirección EVM válida: {e}"))?;

    let provider = ChainProvider::connect(&cfg.chain).await?;
    let addrs = LookupAddresses::from_config(cfg)?;

    let mut app = App::new();
    app.state.search_input = address.trim().to_string();

    let mut view = token_lookup::lookup(&provider, &addrs, token).await?;
    let history = backfill::backfill(
        &provider,
        addrs.pool_manager,
        addrs.factory,
        token,
        &view,
        cfg.indexer.backfill_max_blocks_per_request,
        DEFAULT_CANDLE_SECONDS,
        true,
    )
    .await?;

    view.holders = history.holders;
    view.trades_indexed = Some(history.trades.len());
    view.change_since_first_trade_pct = history.change_pct(view.price_in_pair);
    app.state.selected_token = Some(view);
    app.state.candles = history.candles;
    app.state.search_status = SearchStatus::Done;

    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal.draw(|f| crate::ui::draw(f, &app))?;

    // El buffer de TestBackend se vuelca celda a celda: así se ve exactamente
    // el mismo texto que pintaría la terminal real.
    let buf = terminal.backend().buffer();
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            line.push_str(buf[(x, y)].symbol());
        }
        println!("{}", line.trim_end());
    }
    Ok(())
}

/// `cargo run -- operator <address>`
///
/// Historial de lanzamientos de un deployer de Pons V2. Es la validación
/// contra la chain del paso 2 de `data::operator_tracker`: si las cifras que
/// imprime cuadran con lo que se ve filtrando `TokenLaunched` por `topics[3]`
/// a mano, el backfill del perfil es correcto.
pub async fn run_operator_cli(cfg: &AppConfig, address: &str, limit: usize) -> anyhow::Result<()> {
    use crate::data::operator_tracker::backfill_operator_history;

    let deployer: Address = address
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("{address:?} no es una dirección EVM válida: {e}"))?;

    let provider = ChainProvider::connect(&cfg.chain).await?;
    let addrs = LookupAddresses::from_config(cfg)?;

    println!("historial de lanzamientos de {deployer} en chain {} ...", provider.chain_id);
    let started = std::time::Instant::now();
    let profile = backfill_operator_history(
        &provider,
        addrs.factory,
        deployer,
        cfg.indexer.backfill_max_blocks_per_request,
    )
    .await?;

    println!(
        "\nrango buscado: bloques {} → {}  ({:.1} s)",
        profile.history_from_block,
        profile.history_to_block,
        started.elapsed().as_secs_f64()
    );
    println!("  {:<12} {}", "tipo", profile.kind.label());
    if profile.kind.needs_warning() {
        println!(
            "\n  !! esta dirección TIENE BYTECODE: es un contrato, no una wallet.\n\
             \x20    Caso conocido: Multicall3 (0xca11bde0…ca11) figura como deployer\n\
             \x20    de miles de lanzamientos de terceros. Sus lanzamientos NO son de\n\
             \x20    un solo operador y no tiene sentido vigilar su financiación.\n\
             \x20    Se muestra el perfil igualmente, pero no la metas en la watchlist\n\
             \x20    sin saber qué contrato es."
        );
    }
    println!("  {:<12} {}", "lanzamientos", profile.launches.len());
    if profile.launches.is_empty() {
        println!("\nesta wallet no ha lanzado ningún token con la factory V2.");
        return Ok(());
    }

    match profile.median_interval_secs() {
        Some(s) => println!("  {:<12} {} (mediana entre lanzamientos)", "cadencia", fmt_duration(s)),
        None => println!("  {:<12} n/d (un solo lanzamiento)", "cadencia"),
    }
    let pairs = profile.distinct_pair_tokens();
    println!("  {:<12} {} distintos", "pairToken", pairs.len());
    for (addr, n) in pairs.iter().take(5) {
        println!("      {addr}  ×{n}");
    }

    println!("\núltimos {limit} lanzamientos (más reciente abajo):");
    println!("  {:<12} {:<20} {:<44} {}", "bloque", "fecha (UTC)", "token", "curva");
    let skip = profile.launches.len().saturating_sub(limit);
    for l in profile.launches.iter().skip(skip) {
        println!(
            "  {:<12} {:<20} {:<44} {}",
            l.block,
            fmt_time(l.timestamp),
            l.token.to_string(),
            l.curve
        );
    }
    println!("\n(las fechas pueden venir interpoladas entre bloques ancla: sirven\n\
              para ordenar y medir cadencia, no como dato exacto)");
    Ok(())
}

fn fmt_duration(secs: u64) -> String {
    if secs >= 86400 {
        format!("{:.1} d", secs as f64 / 86400.0)
    } else if secs >= 3600 {
        format!("{:.1} h", secs as f64 / 3600.0)
    } else {
        format!("{:.0} min", secs as f64 / 60.0)
    }
}

/// `cargo run -- funding <address> [ventana_horas]`
///
/// Mide la financiación de una wallet de operador y, sobre todo, **el reparto
/// real ETH-nativo vs ERC-20**: el dato del que el usuario hizo depender el
/// punto (c) del diseño (si la mayoría fuese ETH nativo, había que ir a
/// Blockscout desde el principio).
pub async fn run_funding_cli(cfg: &AppConfig, address: &str, window_hours: u64) -> anyhow::Result<()> {
    use crate::data::operator_tracker::funding::{
        backfill_erc20_fundings, fill_timestamps, find_native_fundings, fundings_before_launch,
        native_vs_erc20,
    };
    use crate::data::operator_tracker::{backfill_operator_history, FundingAsset};

    let wallet: Address = address
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("{address:?} no es una dirección EVM válida: {e}"))?;

    let provider = ChainProvider::connect(&cfg.chain).await?;
    let addrs = LookupAddresses::from_config(cfg)?;
    let chunk = cfg.indexer.backfill_max_blocks_per_request;

    // El historial acota el rango: antes del primer lanzamiento no hay nada
    // que buscar, y da los timestamps con los que cruzar financiación→lanzamiento.
    let profile = backfill_operator_history(&provider, addrs.factory, wallet, chunk).await?;
    println!("\n{}  {}", wallet, profile.kind.label());
    if profile.kind.needs_warning() {
        println!("  !! tiene bytecode: puede ser un relay, no una wallet financiable.");
    }
    println!("  {:<14} {}", "lanzamientos", profile.launches.len());
    let Some(first) = profile.launches.first() else {
        println!("\nsin lanzamientos: no hay ventana que medir.");
        return Ok(());
    };
    // Un margen por debajo del primer lanzamiento: la financiación que lo
    // hizo posible es anterior a él.
    let from_block = first.block.saturating_sub(200_000).max(1);
    let to_block = profile.history_to_block;
    println!("  {:<14} {from_block} → {to_block}", "rango");

    println!("\nfinanciaciones ERC-20 (eth_getLogs, Transfer con topics[2] = wallet) ...");
    let mut fundings = backfill_erc20_fundings(&provider, wallet, from_block, to_block, chunk).await?;
    println!("  {} entradas ERC-20", fundings.len());

    println!("\nfinanciaciones en ETH nativo (bisección de eth_getBalance; requiere\n\
              RPC archive — el público oficial no lo es) ...");
    let started = std::time::Instant::now();
    let scan = find_native_fundings(&provider, wallet, from_block, to_block).await?;
    println!(
        "  {} entradas nativas detectadas  ({} llamadas de saldo, {:.1} s)",
        scan.fundings.len(),
        scan.balance_calls,
        started.elapsed().as_secs_f64()
    );
    if scan.truncated {
        println!("  !! se alcanzó el tope de llamadas: el recuento nativo está INCOMPLETO");
    }
    if scan.native_internal > 0 {
        println!(
            "  {} de ellas sin tx directa en el bloque (llegaron por llamada interna\n     de un contrato: remitente desconocido, no se inventa)",
            scan.native_internal
        );
    }

    fundings.extend(scan.fundings);
    fill_timestamps(&provider, &mut fundings).await?;
    fundings.sort_by_key(|f| f.block);

    let (native, erc20) = native_vs_erc20(&fundings);
    let total = native + erc20;
    println!("\n=== REPARTO (todas las entradas del rango) ===");
    if total == 0 {
        println!("  no se detectó ninguna entrada de fondos.");
    } else {
        println!("  ETH nativo  {native:>4}  ({:.0} %)", 100.0 * native as f64 / total as f64);
        println!("  ERC-20      {erc20:>4}  ({:.0} %)", 100.0 * erc20 as f64 / total as f64);
    }

    // Lo que de verdad importa para la señal: solo las entradas que preceden
    // a un lanzamiento dentro de la ventana. El resto es ruido de uso normal.
    let window = window_hours * 3600;
    let pre = fundings_before_launch(&profile, &fundings, window);
    let (pn, pe) = {
        let n = pre.iter().filter(|f| f.asset.is_native()).count();
        (n, pre.len() - n)
    };
    println!("\n=== SOLO LAS QUE PRECEDEN A UN LANZAMIENTO (ventana {window_hours} h) ===");
    if pre.is_empty() {
        println!("  ninguna: con esta ventana no hay financiación que preceda a un lanzamiento.");
    } else {
        println!("  ETH nativo  {pn:>4}  ({:.0} %)", 100.0 * pn as f64 / pre.len() as f64);
        println!("  ERC-20      {pe:>4}  ({:.0} %)", 100.0 * pe as f64 / pre.len() as f64);
    }

    println!("\núltimas entradas:");
    println!("  {:<12} {:<20} {:>16}  {:<44} {}", "bloque", "fecha (UTC)", "importe", "de", "activo");
    for f in fundings.iter().rev().take(12).rev() {
        let asset = match &f.asset {
            FundingAsset::Native => "ETH nativo".to_string(),
            FundingAsset::Erc20 { token, .. } => token.to_string(),
        };
        println!(
            "  {:<12} {:<20} {:>16}  {:<44} {}",
            f.block,
            fmt_time(f.timestamp),
            fmt_amount(f.amount),
            f.from.map(|a| a.to_string()).unwrap_or_else(|| "¿interna?".into()),
            asset
        );
    }
    println!(
        "\nel recuento nativo es un SUELO: la bisección ve saldo neto, así que una\n\
         entrada compensada por una salida mayor en el mismo tramo no se ve."
    );
    Ok(())
}
