//! La watchlist de operadores: qué wallets se vigilan.
//!
//! Decisión cerrada el 2026-09-17 (opciones A+B del diseño): la **fuente
//! primaria es `watchlist.toml`**, un fichero de texto editable a mano,
//! versionable y con etiqueta y notas por entrada; `cargo run -- watch
//! <addr>` lo edita desde el propio flujo de uso. La base de datos guarda
//! **alertas y resultados, nunca la watchlist** — ver `data::db`.
//!
//! ## Por qué un contrato relay exige `--force` y el resto solo avisa
//!
//! `classify_deployer` distingue wallet, wallet delegada EIP-7702, posible
//! contrato relay y "no comprobado". Solo `PossibleRelayContract` bloquea el
//! alta sin `--force`, y no por purismo sino por **coste asimétrico**: una
//! wallet mal añadida es alguna alerta de más, mientras que un relay es una
//! manguera —Multicall3 figura como deployer de 4.791 lanzamientos de
//! terceros (medido el 2026-09-17)— que inutilizaría el resto de alertas.
//! `Unknown` (el `eth_getCode` falló) avisa y deja pasar: no comprobado no es
//! lo mismo que comprobado y malo, y bloquear por un fallo de RPC sería
//! confundir las dos cosas.
//!
//! Formato:
//!
//! ```toml
//! [[operator]]
//! address = "0x…"
//! label   = "deployer serie X"
//! added   = "2026-09-21"
//! notes   = "3 lanzamientos vistos"
//! ```

use alloy::primitives::Address;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Ruta por defecto, relativa al directorio de trabajo (igual que
/// `config.toml`).
pub const DEFAULT_WATCHLIST_PATH: &str = "watchlist.toml";

/// Cabecera que se reescribe en cada guardado. El fichero se serializa con
/// serde, así que un comentario escrito a mano dentro de una entrada **se
/// pierde** al usar `cargo run -- watch`: lo que se quiera conservar va en
/// `notes`, que sí es un campo.
const HEADER: &str = "\
# watchlist.toml — operadores de launchpad vigilados.
#
# Fuente primaria de la watchlist (ver src/data/operator_tracker/watchlist.rs).
# Editable a mano; `cargo run -- watch <addr>` añade entradas y `cargo run --
# watch remove <addr>` las quita. Al guardarse desde el comando el fichero se
# reescribe entero: los comentarios sueltos se pierden, las notas van en el
# campo `notes`.
";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedOperator {
    /// Dirección normalizada a minúsculas con `0x`. `0xAb…` y `0xab…` son la
    /// misma wallet y no deben poder coexistir como dos entradas.
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Fecha de alta (`YYYY-MM-DD`), informativa.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl WatchedOperator {
    pub fn parsed_address(&self) -> anyhow::Result<Address> {
        self.address.trim().parse::<Address>().map_err(|e| {
            anyhow::anyhow!("la entrada {:?} de la watchlist no es una dirección EVM válida: {e}", self.address)
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Watchlist {
    /// `[[operator]]` en el TOML.
    #[serde(default, rename = "operator")]
    pub operators: Vec<WatchedOperator>,
}

impl Watchlist {
    /// Lee la watchlist. Un fichero inexistente es una lista vacía (el caso
    /// normal la primera vez), pero un fichero **ilegible o mal formado es un
    /// error**: vigilar a nadie por un TOML roto es justo el fallo silencioso
    /// que este proyecto ya pagó caro una vez.
    pub fn load(path: &str) -> anyhow::Result<Self> {
        if !Path::new(path).exists() {
            return Ok(Watchlist::default());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("no se pudo leer {path}: {e}"))?;
        let list: Watchlist = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("{path} no es un TOML de watchlist válido: {e}"))?;
        list.validate(path)?;
        Ok(list)
    }

    /// Toda dirección tiene que parsear y no puede repetirse. Se comprueba al
    /// cargar, no al usar: un duplicado significaría alertas por duplicado y
    /// dos notas distintas para la misma wallet.
    fn validate(&self, path: &str) -> anyhow::Result<()> {
        let mut seen = std::collections::HashSet::new();
        for op in &self.operators {
            let addr = op.parsed_address()?;
            if !seen.insert(addr) {
                anyhow::bail!("{path} tiene la dirección {addr} repetida: quita una de las dos entradas");
            }
        }
        Ok(())
    }

    /// Escribe el fichero entero (cabecera + entradas).
    pub fn save(&self, path: &str) -> anyhow::Result<()> {
        self.validate(path)?;
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("no se pudo serializar la watchlist: {e}"))?;
        std::fs::write(path, format!("{HEADER}\n{body}"))
            .map_err(|e| anyhow::anyhow!("no se pudo escribir {path}: {e}"))?;
        Ok(())
    }

    pub fn contains(&self, address: Address) -> bool {
        self.find(address).is_some()
    }

    pub fn find(&self, address: Address) -> Option<&WatchedOperator> {
        self.operators
            .iter()
            .find(|o| o.parsed_address().map(|a| a == address).unwrap_or(false))
    }

    /// Añade la wallet. Devuelve `false` si ya estaba (no se duplica ni se
    /// machacan su etiqueta y sus notas).
    pub fn add(&mut self, address: Address, label: Option<String>, notes: Option<String>) -> bool {
        if self.contains(address) {
            return false;
        }
        self.operators.push(WatchedOperator {
            address: format!("{address:#x}"),
            label,
            added: Some(today()),
            notes,
        });
        true
    }

    /// Quita la wallet. Devuelve `false` si no estaba.
    pub fn remove(&mut self, address: Address) -> bool {
        let before = self.operators.len();
        self.operators
            .retain(|o| o.parsed_address().map(|a| a != address).unwrap_or(true));
        self.operators.len() != before
    }

    /// Las direcciones ya parseadas, que es lo que consume la vigilancia.
    pub fn addresses(&self) -> anyhow::Result<Vec<Address>> {
        self.operators.iter().map(|o| o.parsed_address()).collect()
    }
}

/// Fecha de hoy en `YYYY-MM-DD`, reutilizando el formateador del CLI para no
/// arrastrar `chrono` por una columna.
fn today() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::cli::fmt_time(now).split(' ').next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    #[test]
    fn fichero_inexistente_es_lista_vacia() {
        let list = Watchlist::load("/tmp/no-existe-jamas-watchlist-marxi.toml").unwrap();
        assert!(list.operators.is_empty());
    }

    #[test]
    fn anadir_es_idempotente_y_no_pisa_la_etiqueta() {
        let mut list = Watchlist::default();
        assert!(list.add(addr(1), Some("serie X".into()), None));
        assert!(!list.add(addr(1), Some("otra".into()), None));
        assert_eq!(list.operators.len(), 1);
        assert_eq!(list.operators[0].label.as_deref(), Some("serie X"));
    }

    #[test]
    fn mayusculas_y_minusculas_son_la_misma_wallet() {
        let mut list = Watchlist::default();
        list.add(addr(0xab), None, None);
        let upper = list.operators[0].address.to_uppercase().replace("0X", "0x");
        let parsed: Address = upper.parse().unwrap();
        assert!(list.contains(parsed), "la comparación debe ser por dirección, no por texto");
    }

    #[test]
    fn quitar_dice_si_estaba() {
        let mut list = Watchlist::default();
        list.add(addr(2), None, None);
        assert!(list.remove(addr(2)));
        assert!(!list.remove(addr(2)));
    }

    #[test]
    fn duplicado_en_el_fichero_es_error_no_silencio() {
        let raw = r#"
[[operator]]
address = "0x0101010101010101010101010101010101010101"
[[operator]]
address = "0x0101010101010101010101010101010101010101"
"#;
        let list: Watchlist = toml::from_str(raw).unwrap();
        assert!(list.validate("test.toml").is_err());
    }

    #[test]
    fn ida_y_vuelta_por_disco() {
        let dir = std::env::temp_dir().join(format!("marxi-watchlist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watchlist.toml");
        let path = path.to_str().unwrap();

        let mut list = Watchlist::default();
        list.add(addr(3), Some("etiqueta".into()), Some("nota".into()));
        list.save(path).unwrap();

        let back = Watchlist::load(path).unwrap();
        assert_eq!(back.operators.len(), 1);
        assert_eq!(back.operators[0].label.as_deref(), Some("etiqueta"));
        assert!(back.contains(addr(3)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
