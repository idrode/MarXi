//! Rastreo de operadores del launchpad: wallets que lanzan tokens en Pons V2
//! de forma repetida.
//!
//! Alcance actual: perfilar una wallet concreta — su **historial de
//! lanzamientos** (paso 2) y su **financiación** en ERC-20 y en ETH nativo
//! (paso 3, señal 1 del diseño), más la **watchlist** (paso 6,
//! `watchlist.toml`) de qué wallets se vigilan. La vigilancia en vivo era el
//! paso 5, descartado el 2026-09-18 al quedar la señal 1 desmontada por los
//! datos; de ella solo sobrevive la señal 2 (`TokenLaunched` de una wallet de
//! la watchlist), implementada el 2026-09-22 en `watcher` y conectada a la
//! TUI (pestañas Operador y Alertas).

pub mod baseline;
pub mod creator;
pub mod funding;
pub mod graduation;
pub mod history;
pub mod profile;
pub mod watcher;
pub mod watchlist;

pub use baseline::{
    LowReason, build_baselines, classify_fundings, summarize, ClassifiedFunding, Confidence,
    FundingKind, IncomeReason,
};
pub use history::{backfill_operator_history, classify_deployer};
pub use watcher::spawn_launch_watcher;
pub use watchlist::{Watchlist, DEFAULT_WATCHLIST_PATH};
pub use profile::{DeployerKind, Funding, FundingAsset, OperatorProfile, PastLaunch};
