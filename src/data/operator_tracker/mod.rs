//! Rastreo de operadores del launchpad: wallets que lanzan tokens en Pons V2
//! de forma repetida.
//!
//! Alcance actual: perfilar una wallet concreta — su **historial de
//! lanzamientos** (paso 2) y su **financiación** en ERC-20 y en ETH nativo
//! (paso 3, señal 1 del diseño). La vigilancia en vivo por WS y las alertas
//! son los pasos 5 y 6, todavía sin implementar.

pub mod funding;
pub mod history;
pub mod profile;

pub use history::backfill_operator_history;
pub use funding::{backfill_erc20_fundings, find_native_fundings, native_vs_erc20};
pub use profile::{DeployerKind, Funding, FundingAsset, OperatorProfile, PastLaunch};
