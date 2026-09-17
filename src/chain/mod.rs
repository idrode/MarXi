//! Todo lo relacionado con la conexión a Robinhood Chain: construcción del
//! provider (alloy), y verificación on-chain de direcciones antes de
//! confiarlas (ver `verify`).
//!
//! Principio rector de este módulo, no negociable: NINGUNA dirección de
//! contrato (factory, router, hook, pool manager) se usa en `trading::` sin
//! haber pasado por `verify` al menos una vez por sesión. Las funciones
//! reales son `verify::has_bytecode` (filtro básico) y las de rol:
//! `confirm_pons_launch_factory`, `confirm_pons_launch_router`,
//! `confirm_pons_meme_hook`, `confirm_uniswap_v4_pool_manager`. Hoy el gate
//! se ejecuta con `cargo run -- verify` (`verify::run_verify_cli`); cuando
//! exista trading real (Fase 2), los motores deben exigir ese mismo
//! resultado antes de firmar.
//! Las direcciones en config.toml pueden estar mal, obsoletas, o ser
//! placeholders — ya nos pasó con HyperSwap/HyperEVM antes de pivotar a esta
//! chain, y las fuentes públicas de direcciones para Pons/Bankr aquí son de
//! documentación de terceros (Bitquery), no la fuente on-chain misma.

pub mod provider;
pub mod verify;

pub use provider::ChainProvider;
