//! Persistencia local del indexador. Mismo patrón que el daemon SQLite de
//! hyperT en el RedMagic: swaps/curve-trades crudos + velas ya agregadas.
//!
//! Nota: no se ha añadido todavía una dependencia de SQLite a Cargo.toml
//! (ej. `rusqlite` o `sqlx`) — pendiente de decidir cuál, a valorar junto al
//! resto del stack cuando se implemente este módulo. No añadir la
//! dependencia especulativamente antes de tener el schema claro.

pub struct Db {
    pub path: String,
}

impl Db {
    pub fn open(_path: &str) -> anyhow::Result<Self> {
        todo!("abrir/crear la base de datos SQLite en `path`, aplicar migraciones de schema")
    }

    // TODO: pub fn insert_raw_trade(...)
    // TODO: pub fn insert_candle(...)
    // TODO: pub fn backfill_range(&self, from_block: u64, to_block: u64) -> ...
}
