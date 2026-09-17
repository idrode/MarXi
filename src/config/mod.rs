//! Carga y validación de `config.toml`. Nunca vive aquí ningún secreto
//! (claves privadas, contraseñas, API keys) — esos vienen de `.env` /
//! variables de entorno o del keystore cifrado (ver `security::keystore`).

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub chain: ChainConfig,
    pub gas: GasConfig,
    pub trading: TradingConfig,
    pub launchpads: Vec<LaunchpadConfig>,
    pub uniswap_v4: UniswapV4Config,
    pub security: SecurityConfig,
    pub indexer: IndexerConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainConfig {
    pub name: String,
    pub chain_id: u64,
    pub rpc_http_fallback: String,
    pub rpc_provider: String,
    pub explorer_url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GasConfig {
    pub max_priority_fee_gwei: f64,
    pub gas_limit_margin_pct: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TradingConfig {
    pub default_slippage_bps: u32,
    pub tx_deadline_seconds: u64,
    pub approve_max_multiplier: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LaunchpadConfig {
    pub name: String,
    pub enabled: bool,
    #[serde(default)]
    pub version: Option<String>,
    pub launch_factory: String,
    pub launch_router: String,
    pub graduation_hook: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct UniswapV4Config {
    pub pool_manager: String,
    pub position_manager: String,
    pub state_view: String,
    pub quoter: String,
    pub universal_router: String,
    pub permit2: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SecurityConfig {
    pub keystore_path: String,
    pub honeypot_check_provider: String,
    pub simulate_before_send: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct IndexerConfig {
    pub db_path: String,
    pub backfill_max_blocks_per_request: u64,
}

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path.as_ref()).map_err(|e| {
            anyhow::anyhow!(
                "no se pudo leer {:?} (¿copiaste config.example.toml a config.toml?): {e}",
                path.as_ref()
            )
        })?;
        let cfg: AppConfig = toml::from_str(&raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validaciones mínimas de cordura. No sustituye a verificar direcciones
    /// on-chain (eso es responsabilidad de `chain::verify` antes de operar,
    /// no de este parseo de config).
    fn validate(&self) -> anyhow::Result<()> {
        if self.trading.default_slippage_bps > 5000 {
            anyhow::bail!("default_slippage_bps > 5000 (50%) parece un error de config, no un valor real");
        }
        if !self.security.simulate_before_send {
            tracing::warn!(
                "simulate_before_send está desactivado — se enviarán transacciones sin simulación previa"
            );
        }

        // Toda dirección que vaya a usarse debe ser al menos hex válido de 20
        // bytes. Esto NO la verifica on-chain (eso es `chain::verify`), pero
        // evita que un placeholder como "0x8366a39cPENDIENTE..." cargue sin
        // error, que es exactamente lo que pasaba antes.
        let v4 = &self.uniswap_v4;
        for (name, addr) in [
            ("uniswap_v4.pool_manager", &v4.pool_manager),
            ("uniswap_v4.position_manager", &v4.position_manager),
            ("uniswap_v4.state_view", &v4.state_view),
            ("uniswap_v4.quoter", &v4.quoter),
            ("uniswap_v4.universal_router", &v4.universal_router),
            ("uniswap_v4.permit2", &v4.permit2),
        ] {
            check_address(name, addr)?;
        }
        for lp in &self.launchpads {
            if !lp.enabled {
                continue;
            }
            for (field, addr) in [
                ("launch_factory", &lp.launch_factory),
                ("launch_router", &lp.launch_router),
                ("graduation_hook", &lp.graduation_hook),
            ] {
                check_address(&format!("launchpads[{}].{field}", lp.name), addr)?;
            }
        }
        Ok(())
    }
}

fn check_address(name: &str, addr: &str) -> anyhow::Result<()> {
    addr.parse::<alloy::primitives::Address>()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("config: {name} = {addr:?} no es una dirección EVM válida: {e}"))
}
