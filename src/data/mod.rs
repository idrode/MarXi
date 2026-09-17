//! Indexador propio: escucha eventos on-chain vía WebSocket y los convierte
//! en datos utilizables (velas OHLCV, lanzamientos/graduaciones detectadas,
//! metadata de tokens). No existe un "candle feed" pre-hecho para un
//! memecoin recién creado en Robinhood Chain — se construye aquí, evento a
//! evento, igual que se diseñó para el proyecto original en HyperEVM.

pub mod abi;
pub mod backfill;
pub mod watcher;
pub mod candles;
pub mod operator_tracker;
pub mod token_lookup;
pub mod token_metadata;
pub mod db;
