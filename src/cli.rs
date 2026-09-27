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
    cache_profile(cfg, &profile, None);
    Ok(())
}

pub fn fmt_duration(secs: u64) -> String {
    if secs >= 86400 {
        format!("{:.1} d", secs as f64 / 86400.0)
    } else if secs >= 3600 {
        format!("{:.1} h", secs as f64 / 3600.0)
    } else {
        format!("{:.0} min", secs as f64 / 60.0)
    }
}

/// `cargo run -- funding <address> [ventana_horas] [--from-block N]`
///
/// Mide la financiación de una wallet de operador y, sobre todo, **el reparto
/// real ETH-nativo vs ERC-20**: el dato del que el usuario hizo depender el
/// punto (c) del diseño (si la mayoría fuese ETH nativo, había que ir a
/// Blockscout desde el principio).
///
/// `from_block` fija el inicio del rango a mano. Hace falta para creadores que
/// no aparecen como `deployer` de `TokenLaunched` (lanzaron por un contrato
/// intermediario o en otro launchpad): sin él no hay primer lanzamiento con el
/// que acotar el rango y el comando no mide nada.
pub async fn run_funding_cli(
    cfg: &AppConfig,
    address: &str,
    window_hours: u64,
    from_block: Option<u64>,
) -> anyhow::Result<()> {
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
    let manual_range = from_block.is_some();
    let from_block = match (from_block, profile.launches.first()) {
        (Some(n), _) => n,
        // Un margen por debajo del primer lanzamiento: la financiación que lo
        // hizo posible es anterior a él.
        (None, Some(first)) => first.block.saturating_sub(200_000).max(1),
        (None, None) => {
            println!(
                "\nsin lanzamientos como deployer de Pons V2: no hay ventana que medir.\n\
                 Si creó su token por un contrato intermediario o en otro launchpad,\n\
                 pasa el rango a mano: --from-block <bloque>."
            );
            return Ok(());
        }
    };
    let to_block = profile.history_to_block;
    if profile.launches.is_empty() {
        println!(
            "  !! rango manual sin lanzamientos propios en V2: no hay retardo hasta el\n     \
             siguiente lanzamiento, y ningún ERC-20 cuenta como activo gastable\n     \
             (la condición 4 usa sus pairTokens), así que todo ERC-20 sale en BAJA."
        );
    }
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

    // Criterio aprobado el 2026-09-18: separar financiación de arranque de
    // ingreso del negocio. Sin esto las cifras de arriba se leen mal — la
    // inmensa mayoría de las entradas son ingresos, no financiación.
    let classified = print_classification(&provider, &profile, fundings).await?;
    if manual_range && profile.launches.is_empty() {
        // Un perfil sin lanzamientos y con un rango elegido a mano no es un
        // snapshot de operador: no se mezcla con la caché.
        println!("\n(rango manual sin lanzamientos propios: no se cachea)");
    } else {
        cache_profile(cfg, &profile, Some(&classified));
    }
    Ok(())
}

/// Aplica el criterio de las cuatro condiciones y construye el baseline.
async fn print_classification(
    provider: &ChainProvider,
    profile: &crate::data::operator_tracker::OperatorProfile,
    fundings: Vec<crate::data::operator_tracker::Funding>,
) -> anyhow::Result<Vec<crate::data::operator_tracker::ClassifiedFunding>> {
    use crate::data::operator_tracker::baseline::{
        build_baselines, classify_fundings, summarize, Confidence, FundingKind,
        MIN_BASELINE_SAMPLES,
    };
    use crate::data::operator_tracker::FundingAsset;

    println!(
        "\nclasificando entradas (criterio de las 4 condiciones: no la inicia él,\n\
         remitente EOA, nativa interna nunca, activo gastable) ..."
    );
    let started = std::time::Instant::now();
    let classified = classify_fundings(provider, profile, fundings).await?;
    let s = summarize(&classified);
    println!("  ({:.1} s)", started.elapsed().as_secs_f64());

    println!("\n=== FINANCIACIÓN DE ARRANQUE vs INGRESO DEL NEGOCIO ===");
    println!("  {:<34} {:>6}", "entradas totales", s.total);
    println!("  {:<34} {:>6}", "financiación (confianza alta)", s.startup_high);
    println!("  {:<34} {:>6}  (airdrop o remitente-relay)", "financiación (confianza baja)", s.startup_low);
    println!("  {:<34} {:>6}", "ingreso: la inició él mismo", s.self_initiated);
    println!("  {:<34} {:>6}", "ingreso: remitente con bytecode", s.sender_is_contract);
    println!("  {:<34} {:>6}", "ingreso: nativa interna", s.native_internal);
    if s.not_evaluated > 0 {
        println!("  {:<34} {:>6}  !! no comprobado ≠ ingreso", "sin evaluar", s.not_evaluated);
    }

    let startup: Vec<_> = classified.iter().filter(|c| c.is_startup()).collect();
    if startup.is_empty() {
        println!("\n  ninguna entrada pasa el criterio: esta wallet se autofinancia\n\
                  con sus propios ingresos en todo el rango mirado.");
    } else {
        println!("\n  entradas que pasan el criterio:");
        println!("    {:<12} {:<20} {:>14}  {:<44} {}", "bloque", "fecha (UTC)", "importe", "de", "nota");
        for c in &startup {
            let conf = match c.kind {
                FundingKind::Startup(conf) => conf.label(),
                FundingKind::Income(_) => unreachable!("filtrado por is_startup"),
            };
            let pre = if c.precedes_first_launch { " · ANTES DEL PRIMER LANZAMIENTO" } else { "" };
            let infra = match c.sender_nonce {
                Some(n) if n >= crate::data::operator_tracker::baseline::INFRA_NONCE_THRESHOLD => {
                    format!(" · remitente con nonce {n}: INFRAESTRUCTURA COMPARTIDA, no wallet madre")
                }
                Some(n) => format!(" · nonce del remitente {n}"),
                None => String::new(),
            };
            let delay = c
                .delay_to_next_launch
                .map(|d| format!(" · +{} al siguiente launch", fmt_duration(d)))
                .unwrap_or_default();
            println!(
                "    {:<12} {:<20} {:>14}  {:<44} {conf}{pre}{delay}{infra}",
                c.funding.block,
                fmt_time(c.funding.timestamp),
                fmt_amount(c.funding.amount),
                c.funding.from.map(|a| a.to_string()).unwrap_or_else(|| "?".into()),
            );
        }
    }

    let baselines = build_baselines(&classified);
    println!("\n=== BASELINE ===");
    if baselines.is_empty() {
        println!("  sin muestras de confianza alta: no hay baseline. Es el caso normal\n\
                  con este criterio, no un fallo.");
    }
    for b in &baselines {
        let asset = match &b.asset {
            FundingAsset::Native => "ETH nativo".to_string(),
            FundingAsset::Erc20 { token, .. } => token.to_string(),
        };
        println!(
            "  {asset}: {} muestra(s)  min {}  mediana {}  max {}",
            b.samples,
            fmt_amount(b.min),
            fmt_amount(b.median),
            fmt_amount(b.max)
        );
        if b.samples < MIN_BASELINE_SAMPLES {
            println!(
                "    con menos de {MIN_BASELINE_SAMPLES} muestras no se compara nada:\n\
                 \x20   una financiación nueva saldrá como NoBaseline, y la alerta se\n\
                 \x20   emite igual diciéndolo."
            );
        }
        if !b.recurring_sources.is_empty() {
            println!("    remitentes recurrentes (candidatos a wallet madre):");
            for a in &b.recurring_sources {
                println!("      {a}");
            }
        }
    }
    println!(
        "\nel retardo hasta el siguiente lanzamiento se imprime como DATO, nunca se\n\
         usa como filtro: medido que con cadencias de ~2 min no discrimina nada."
    );
    Ok(classified)
}

/// Guarda el perfil en la caché de `data::db` y dice qué había antes.
///
/// Se avisa por pantalla en vez de hacerlo callando: un snapshot cacheado
/// tiene una antigüedad y un rango de bloques concretos, y quien lo lea
/// después tiene que saberlo.
fn cache_profile(
    cfg: &AppConfig,
    profile: &crate::data::operator_tracker::OperatorProfile,
    classified: Option<&[crate::data::operator_tracker::ClassifiedFunding]>,
) {
    let mut db = match crate::data::db::Db::open(&cfg.indexer.db_path) {
        Ok(db) => db,
        Err(e) => {
            // No es motivo para tirar la consulta: el dato ya se ha impreso.
            println!("\n(no se pudo abrir la caché en {}: {e})", cfg.indexer.db_path);
            return;
        }
    };
    let previo = db.operator_cache_info(profile.address).ok().flatten();
    match db.save_operator_profile(profile, classified) {
        Ok(()) => {
            print!("\ncacheado en {} ({} lanzamientos", cfg.indexer.db_path, profile.launches.len());
            match classified {
                Some(c) => println!(", {} financiaciones clasificadas)", c.len()),
                None => println!(", financiación sin tocar)"),
            }
            if let Some(p) = previo {
                println!(
                    "  el snapshot anterior era de {} y cubría hasta el bloque {}",
                    fmt_time(p.cached_at),
                    p.history_to_block
                );
            }
        }
        Err(e) => println!("\n(no se pudo cachear el perfil: {e})"),
    }
}

/// `cargo run -- alert list [n]` / `cargo run -- alert <id> <outcome> [nota]`
///
/// El ciclo de prueba y error del diseño: una alerta se registra al emitirse y
/// se cierra después con lo que pasó de verdad. El vocabulario de `outcome` es
/// libre a propósito — se decide con alertas reales delante, no antes.
pub fn run_alert_cli(cfg: &AppConfig, args: &[String]) -> anyhow::Result<()> {
    let db = crate::data::db::Db::open(&cfg.indexer.db_path)?;
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let limit = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(20);
            let rows = db.recent_alerts(limit, false)?;
            if rows.is_empty() {
                println!("no hay alertas registradas todavía en {}.", cfg.indexer.db_path);
                return Ok(());
            }
            println!("{:<6} {:<18} {:<20} {:<44} {}", "id", "tipo", "fecha (UTC)", "wallet", "resultado");
            for r in rows.iter().rev() {
                println!(
                    "{:<6} {:<18} {:<20} {:<44} {}",
                    r.id,
                    r.kind,
                    fmt_time(r.created_at),
                    r.address,
                    r.outcome.clone().unwrap_or_else(|| "PENDIENTE".into())
                );
            }
            let pend = db.recent_alerts(usize::MAX.min(10_000), true)?.len();
            if pend > 0 {
                println!("\n{pend} sin resultado. Ciérralas con: cargo run -- alert <id> <outcome> [nota]");
            }
            Ok(())
        }
        Some(id_str) => {
            let id: i64 = id_str.parse().map_err(|_| {
                anyhow::anyhow!("uso: cargo run -- alert list [n]  |  cargo run -- alert <id> <outcome> [nota]")
            })?;
            let outcome = args.get(1).ok_or_else(|| {
                anyhow::anyhow!("falta el resultado: cargo run -- alert <id> <outcome> [nota]")
            })?;
            let note = if args.len() > 2 { Some(args[2..].join(" ")) } else { None };
            db.set_alert_outcome(id, outcome, note.as_deref())?;
            println!("alerta {id} marcada como {outcome}.");
            Ok(())
        }
    }
}

/// `cargo run -- watch [<addr> [--label L] [--notes N] [--force] | remove <addr>]`
///
/// Paso 6 de `operator_tracker`: gestiona `watchlist.toml`, la fuente
/// primaria de qué wallets se vigilan (la DB guarda alertas, no la lista).
///
/// Antes de dar de alta una dirección se clasifica on-chain con
/// `classify_deployer`. Un **posible contrato relay exige `--force`**: el
/// coste de equivocarse es asimétrico —Multicall3 figura como deployer de
/// 4.791 lanzamientos de terceros—, así que un relay colado en la lista no
/// sería una alerta de más sino una manguera que taparía todas las demás. El
/// resto de casos, `Unknown` incluido, avisan y dejan pasar: un `eth_getCode`
/// que falla no es prueba de nada.
pub async fn run_watch_cli(cfg: &AppConfig, args: &[String]) -> anyhow::Result<()> {
    use crate::data::operator_tracker::{classify_deployer, Watchlist, DEFAULT_WATCHLIST_PATH};

    let path = DEFAULT_WATCHLIST_PATH;
    let mut list = Watchlist::load(path)?;

    // El subcomando se acepta con guiones o sin ellos (`list` y `--list` son
    // lo mismo): quien escribe `--list` está pidiendo la lista, no dando una
    // dirección, y hacerle leer un error de parseo de direcciones por la
    // forma de escribirlo no aporta nada.
    let head = args.first().map(|a| a.trim_start_matches('-'));

    match head {
        // Sin argumentos: mostrar la lista.
        None | Some("list") | Some("ls") => {
            print_watchlist(&list, path);
            Ok(())
        }
        Some("help") | Some("h") => {
            print_watch_usage();
            Ok(())
        }
        Some("remove") | Some("rm") => {
            let addr: Address = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("uso: cargo run -- watch remove <dirección>"))?
                .trim()
                .parse()
                .map_err(|e| anyhow::anyhow!("no es una dirección EVM válida: {e}"))?;
            if !list.remove(addr) {
                anyhow::bail!("{addr} no estaba en {path}");
            }
            list.save(path)?;
            println!("{addr} quitada de {path}. Quedan {}.", list.operators.len());
            Ok(())
        }
        // Cualquier otra cosa que empiece por guion no es una dirección: es
        // una opción mal escrita, y se dice así en vez de intentar parsearla.
        Some(_) if args[0].starts_with('-') => {
            print_watch_usage();
            anyhow::bail!("opción desconocida {:?} para `watch`", args[0]);
        }
        Some(first) => {
            let addr: Address = first
                .trim()
                .parse()
                .map_err(|e| anyhow::anyhow!("{first:?} no es una dirección EVM válida: {e}"))?;

            let mut force = false;
            let mut label = None;
            let mut notes = None;
            let mut rest = args[1..].iter();
            while let Some(flag) = rest.next() {
                match flag.as_str() {
                    "--force" | "-f" => force = true,
                    "--label" | "-l" => {
                        label = Some(rest.next().cloned().ok_or_else(|| {
                            anyhow::anyhow!("--label necesita un valor")
                        })?)
                    }
                    "--notes" | "-n" => {
                        notes = Some(rest.next().cloned().ok_or_else(|| {
                            anyhow::anyhow!("--notes necesita un valor")
                        })?)
                    }
                    other => anyhow::bail!(
                        "opción desconocida {other:?}. Uso: cargo run -- watch <addr> \
                         [--label L] [--notes N] [--force]"
                    ),
                }
            }

            if let Some(existing) = list.find(addr) {
                println!(
                    "{addr} ya estaba en {path}{}.",
                    existing
                        .label
                        .as_ref()
                        .map(|l| format!(" como {l:?}"))
                        .unwrap_or_default()
                );
                print_watchlist(&list, path);
                return Ok(());
            }

            // Clasificar antes de dar de alta: es una sola llamada y decide si
            // esta dirección puede entrar sin --force.
            let provider = ChainProvider::connect(&cfg.chain).await?;
            println!("comprobando qué es {addr} en chain {} ...", provider.chain_id);
            let kind = classify_deployer(&provider, addr).await;
            println!("  tipo: {}", kind.label());

            if kind == crate::data::operator_tracker::DeployerKind::PossibleRelayContract && !force {
                anyhow::bail!(
                    "{addr} tiene bytecode y no es una delegación EIP-7702: parece un contrato\n\
                     relay, no una wallet de operador. No se añade.\n\n\
                     Un relay en la watchlist no es una alerta de más: Multicall3 figura como\n\
                     deployer de 4.791 lanzamientos de terceros, así que taparía el resto de\n\
                     alertas. Si sabes qué contrato es y aun así lo quieres vigilar:\n\n    \
                     cargo run -- watch {addr} --force"
                );
            }
            if kind.needs_warning() {
                // Aquí solo puede quedar Unknown, o un relay con --force.
                println!(
                    "\n  !! se añade igualmente, pero esta dirección NO está confirmada como\n\
                     \x20    wallet de operador. Revisa sus alertas antes de fiarte de ellas."
                );
            }

            list.add(addr, label, notes);
            list.save(path)?;
            println!("\n{addr} añadida a {path}.");
            print_watchlist(&list, path);
            println!(
                "\n(esta lista la consume la vigilancia de la señal 2 —`TokenLaunched` de una\n\
                 wallet vigilada—, que arranca sola al abrir la TUI con `cargo run`. Las\n\
                 alertas se ven en su pestaña y quedan registradas en la base: ciérralas\n\
                 con `cargo run -- alert <id> <outcome>`. La señal 1, financiación previa,\n\
                 sigue descartada desde el 2026-09-18.)"
            );
            Ok(())
        }
    }
}

fn print_watch_usage() {
    println!(
        "uso:\n  \
         cargo run -- watch                      lista los operadores vigilados\n  \
         cargo run -- watch <addr> [opciones]    da de alta una wallet\n  \
         cargo run -- watch remove <addr>        da de baja una wallet\n\n\
         opciones del alta:\n  \
         --label <texto>   etiqueta corta del operador\n  \
         --notes <texto>   notas libres (lo que se quiera conservar va aquí:\n                    \
         el fichero se reescribe al guardar)\n  \
         --force           añade una dirección con bytecode (posible relay).\n                    \
         Sin esto se rechaza: un relay tapa el resto de alertas.\n\n\
         (`list`, `remove` y `help` se aceptan también con guiones: --list, --help)"
    );
}

fn print_watchlist(list: &crate::data::operator_tracker::Watchlist, path: &str) {
    if list.operators.is_empty() {
        println!("la watchlist ({path}) está vacía. Añade una con: cargo run -- watch <dirección>");
        return;
    }
    println!("\nwatchlist ({path}) — {} operadores:", list.operators.len());
    println!("  {:<44} {:<12} {}", "dirección", "alta", "etiqueta");
    for op in &list.operators {
        println!(
            "  {:<44} {:<12} {}",
            op.address,
            op.added.as_deref().unwrap_or("-"),
            op.label.as_deref().unwrap_or("")
        );
        if let Some(n) = &op.notes {
            println!("      {n}");
        }
    }
}
