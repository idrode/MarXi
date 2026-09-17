mod app;
mod chain;
mod cli;
mod config;
mod data;
mod security;
mod trading;
mod ui;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install().ok();
    dotenvy::dotenv().ok(); // no falla si no hay .env, solo en producción real conviene exigirlo

    // La TUI toma la pantalla entera: si los logs fueran a stdout o stderr la
    // corromperían. En modo TUI van a un fichero; en los subcomandos de texto
    // van a stderr, para no mezclarse con la salida que se lee o se redirige.
    let is_tui = matches!(std::env::args().nth(1).as_deref(), None);
    let filter = tracing_subscriber::EnvFilter::from_default_env();
    if is_tui {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("marxi.log")
            .map_err(|e| anyhow::anyhow!("no se pudo abrir marxi.log para los logs de la TUI: {e}"))?;
        tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(log).init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
    }

    // Orden de arranque intencional:
    // 1. Cargar config.toml (sin secretos)
    // 2. Cargar .env / variables de entorno (secretos no sensibles: API keys de RPC)
    // 3. Desbloquear el keystore de forma INTERACTIVA (nunca leer la contraseña de un
    //    argumento de CLI ni loguearla) — ver security::keystore
    // 4. Construir el AppState y arrancar el loop de la TUI

    let cfg = config::AppConfig::load("config.toml")?;

    // Subcomando de Fase 0: `cargo run -- verify` conecta al RPC y verifica
    // on-chain las direcciones de config. No arranca la TUI.
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        // Gate de Fase 0: verifica on-chain las direcciones de config.
        Some("verify") => return chain::verify::run_verify_cli(&cfg).await,
        // Fase 1: buscador por-token en modo texto.
        Some("token") => {
            let addr = args.get(2).ok_or_else(|| {
                anyhow::anyhow!("uso: cargo run -- token <dirección del token>")
            })?;
            // Tercer argumento opcional: segundos por vela (default 3600).
            let candle_seconds = args
                .get(3)
                .map(|s| s.parse::<u64>())
                .transpose()
                .map_err(|e| anyhow::anyhow!("el tamaño de vela debe ser un número de segundos: {e}"))?
                .unwrap_or(cli::DEFAULT_CANDLE_SECONDS);
            return cli::run_token_cli(&cfg, addr, candle_seconds).await;
        }
        // Fase 1 / operator_tracker: historial de lanzamientos de un deployer.
        Some("operator") => {
            let addr = args.get(2).ok_or_else(|| {
                anyhow::anyhow!("uso: cargo run -- operator <dirección del deployer> [nº a listar]")
            })?;
            let limit = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(20);
            return cli::run_operator_cli(&cfg, addr, limit).await;
        }
        // operator_tracker paso 3: financiación de una wallet y reparto
        // ETH-nativo vs ERC-20.
        Some("funding") => {
            let addr = args.get(2).ok_or_else(|| {
                anyhow::anyhow!("uso: cargo run -- funding <dirección> [ventana en horas]")
            })?;
            let hours = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(24);
            return cli::run_funding_cli(&cfg, addr, hours).await;
        }
        // Render del panel a texto, para validar la TUI sin terminal.
        Some("preview") => {
            let addr = args.get(2).ok_or_else(|| {
                anyhow::anyhow!("uso: cargo run -- preview <dirección> [ancho] [alto]")
            })?;
            let w = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(100);
            let h = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(32);
            return cli::run_preview_cli(&cfg, addr, w, h).await;
        }
        _ => {}
    }

    // TODO(fase 2): desbloquear keystore vía prompt interactivo (rpassword)

    tracing::info!(chain_id = cfg.chain.chain_id, "arrancando robinhood-sniper");

    // Sin subcomando: arranca la TUI. La pestaña por defecto es el buscador
    // por-token, que es el flujo que el usuario quiere usar primero.
    ui::run(app::App::new(), cfg).await
}
