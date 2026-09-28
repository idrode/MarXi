//! Persistencia local (SQLite vía `rusqlite`, feature `bundled`).
//!
//! Alcance de este módulo, deliberadamente estrecho (paso 4 del orden de
//! implementación de `operator_tracker`):
//!
//! - **cachear el perfil de un operador** (historial de lanzamientos y
//!   financiaciones ya clasificadas), porque calcularlo cuesta un backfill
//!   caro y repetirlo en cada consulta es lo que describe la deuda nº13;
//! - **registrar alertas y su resultado**, que es el ciclo de prueba y error
//!   sin el cual el módulo no se puede evaluar.
//!
//! Lo que este módulo **no** guarda, a propósito: la watchlist (fuente
//! primaria es `watchlist.toml`, decidido el 2026-09-17) y los trades/velas
//! crudos del buscador por-token (esa es la deuda nº13 y tendrá su propio
//! schema cuando toque).
//!
//! ## Por qué `alert` tiene un solo tipo de alerta hoy
//!
//! El diseño original preveía dos señales con la misma relevancia:
//! financiación previa ("posible lanzamiento inminente") y `TokenLaunched` de
//! una wallet vigilada ("lanzamiento confirmado"). Lo medido el 2026-09-18
//! **desmontó la señal 1** en sus dos patas: un operador ya conocido se
//! autofinancia con sus ingresos, y sus financiadores no son wallets madre
//! sino relays compartidos. Queda en pie la señal 2.
//!
//! Consecuencia para el schema: `alert.kind` es un **texto libre con un
//! vocabulario controlado en Rust** (`AlertKind`), del que hoy existe un solo
//! valor. No hay columnas propias de ninguna señal —lo específico va en
//! `payload_json`—, así que añadir una señal 1 nueva el día que se encuentre
//! una que funcione es añadir una variante al enum, sin migración. Tampoco se
//! crean tablas ni índices para esa señal hipotética: eso sería diseñar para
//! algo que hoy no existe.
//!
//! Sincronía: `rusqlite` es síncrono a propósito (decisión cerrada
//! 2026-09-10). Las llamadas van dentro de tareas `tokio::spawn` propias, y
//! si alguna query tarda se envuelve en `spawn_blocking`. Nunca desde el
//! render loop.

use crate::data::operator_tracker::{
    ClassifiedFunding, Confidence, FundingAsset, FundingKind, IncomeReason, LowReason,
    OperatorProfile, PastLaunch,
};
use alloy::primitives::{Address, B256, U256};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Versión de schema aplicada. Se compara con `PRAGMA user_version`; cada
/// incremento añade su bloque en `migrate`.
const SCHEMA_VERSION: i64 = 1;

pub struct Db {
    conn: Connection,
    pub path: String,
}

impl Db {
    /// Abre (o crea) la base en `path`, creando el directorio padre si hace
    /// falta, y aplica las migraciones pendientes.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    anyhow::anyhow!("no se pudo crear el directorio {parent:?} para la base: {e}")
                })?;
            }
        }
        let conn = Connection::open(path)
            .map_err(|e| anyhow::anyhow!("no se pudo abrir la base en {path}: {e}"))?;
        // WAL: el futuro watcher escribe mientras la TUI lee.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Db { conn, path: path.to_string() };
        db.migrate()?;
        Ok(db)
    }

    /// Base en memoria, para tests. Mismo schema.
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Db { conn, path: ":memory:".to_string() };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> anyhow::Result<()> {
        let version: i64 =
            self.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0);
        if version > SCHEMA_VERSION {
            anyhow::bail!(
                "la base en {} tiene schema v{version} y este binario conoce hasta la v{SCHEMA_VERSION}: \
                 es de una versión más nueva, no se toca",
                self.path
            );
        }
        if version < 1 {
            self.conn.execute_batch(SCHEMA_V1)?;
        }
        self.conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    // ---------------------------------------------------------------
    // Perfil de operador (caché del backfill)
    // ---------------------------------------------------------------

    /// Guarda el perfil y sus financiaciones ya clasificadas, reemplazando el
    /// snapshot anterior de esa dirección.
    ///
    /// Se borra y reinserta en vez de fusionar: un perfil es el resultado de
    /// un backfill sobre un rango de bloques concreto, y mezclar dos rangos
    /// distintos daría una lista cuya cobertura real ya no sabría nadie.
    /// `history_from_block`/`history_to_block` dicen qué cubre este snapshot.
    ///
    /// `fundings` es `Option` y no una lista vacía por una razón concreta:
    /// `cargo run -- operator` calcula el historial pero **no** la
    /// financiación, y pasarle un slice vacío borraría en silencio lo que
    /// hubiera cacheado un `funding` anterior. `None` = no tocar esa tabla.
    pub fn save_operator_profile(
        &mut self,
        profile: &OperatorProfile,
        fundings: Option<&[ClassifiedFunding]>,
    ) -> anyhow::Result<()> {
        let addr = addr_key(profile.address);
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO operator (address, label, kind, history_from_block, history_to_block, cached_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(address) DO UPDATE SET
                label = excluded.label,
                kind = excluded.kind,
                history_from_block = excluded.history_from_block,
                history_to_block = excluded.history_to_block,
                cached_at = excluded.cached_at",
            params![
                addr,
                profile.label,
                profile.kind.label(),
                profile.history_from_block as i64,
                profile.history_to_block as i64,
                now_secs() as i64,
            ],
        )?;
        tx.execute("DELETE FROM past_launch WHERE address = ?1", params![addr])?;
        if fundings.is_some() {
            tx.execute("DELETE FROM funding WHERE address = ?1", params![addr])?;
        }

        for l in &profile.launches {
            tx.execute(
                "INSERT INTO past_launch
                   (address, token, curve, pair_token, graduation_threshold, block, ts, tx_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    addr,
                    addr_key(l.token),
                    addr_key(l.curve),
                    addr_key(l.pair_token),
                    l.graduation_threshold.to_string(),
                    l.block as i64,
                    l.timestamp as i64,
                    l.tx_hash.to_string(),
                ],
            )?;
        }

        for c in fundings.unwrap_or(&[]) {
            let (asset_kind, asset_token, asset_decimals) = match &c.funding.asset {
                FundingAsset::Native => ("native", None, None),
                FundingAsset::Erc20 { token, decimals } => {
                    ("erc20", Some(addr_key(*token)), Some(*decimals as i64))
                }
            };
            let (kind, kind_detail) = encode_funding_kind(&c.kind);
            tx.execute(
                "INSERT INTO funding
                   (address, from_addr, asset_kind, asset_token, asset_decimals, amount_raw, amount,
                    block, ts, tx_hash, kind, kind_detail, sender_nonce, delay_to_next_launch,
                    precedes_first_launch)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    addr,
                    c.funding.from.map(addr_key),
                    asset_kind,
                    asset_token,
                    asset_decimals,
                    c.funding.amount_raw.to_string(),
                    c.funding.amount,
                    c.funding.block as i64,
                    c.funding.timestamp as i64,
                    c.funding.tx_hash.map(|h| h.to_string()),
                    kind,
                    kind_detail,
                    c.sender_nonce.map(|n| n as i64),
                    c.delay_to_next_launch.map(|d| d as i64),
                    c.precedes_first_launch as i64,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Qué hay cacheado de esta dirección, sin traerlo entero: sirve para
    /// decidir si merece la pena repetir el backfill y para decirle al usuario
    /// **qué antigüedad tiene** lo que se le enseña.
    pub fn operator_cache_info(&self, address: Address) -> anyhow::Result<Option<CacheInfo>> {
        let info = self
            .conn
            .query_row(
                "SELECT o.kind, o.history_from_block, o.history_to_block, o.cached_at,
                        (SELECT COUNT(*) FROM past_launch p WHERE p.address = o.address),
                        (SELECT COUNT(*) FROM funding f WHERE f.address = o.address)
                 FROM operator o WHERE o.address = ?1",
                params![addr_key(address)],
                |r| {
                    Ok(CacheInfo {
                        kind_label: r.get(0)?,
                        history_from_block: r.get::<_, i64>(1)? as u64,
                        history_to_block: r.get::<_, i64>(2)? as u64,
                        cached_at: r.get::<_, i64>(3)? as u64,
                        launches: r.get::<_, i64>(4)? as usize,
                        fundings: r.get::<_, i64>(5)? as usize,
                    })
                },
            )
            .optional()?;
        Ok(info)
    }

    /// Lanzamientos cacheados, en orden cronológico.
    ///
    /// Devuelve `PastLaunch`, el mismo tipo que produce el backfill, para que
    /// quien los consuma no tenga que saber de dónde vinieron. El `timestamp`
    /// puede ser interpolado (igual que al traerlo de la chain): vale para
    /// ordenar y medir cadencia, no como dato de auditoría.
    pub fn load_operator_launches(&self, address: Address) -> anyhow::Result<Vec<PastLaunch>> {
        let mut stmt = self.conn.prepare(
            "SELECT token, curve, pair_token, graduation_threshold, block, ts, tx_hash
             FROM past_launch WHERE address = ?1 ORDER BY block ASC",
        )?;
        let rows = stmt.query_map(params![addr_key(address)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (token, curve, pair, threshold, block, ts, tx) = row?;
            out.push(PastLaunch {
                token: parse_addr(&token)?,
                curve: parse_addr(&curve)?,
                pair_token: parse_addr(&pair)?,
                graduation_threshold: threshold
                    .parse::<U256>()
                    .map_err(|e| anyhow::anyhow!("graduation_threshold corrupto en la base: {e}"))?,
                block: block as u64,
                timestamp: ts as u64,
                tx_hash: tx
                    .parse::<B256>()
                    .map_err(|e| anyhow::anyhow!("tx_hash corrupto en la base: {e}"))?,
            });
        }
        Ok(out)
    }

    // ---------------------------------------------------------------
    // Alertas y su resultado (el ciclo de prueba y error)
    // ---------------------------------------------------------------

    /// Registra una alerta y devuelve su `id`, que es el identificador corto
    /// con el que el usuario marcará después qué pasó con ella.
    pub fn record_alert(&self, alert: &NewAlert) -> anyhow::Result<i64> {
        self.conn.execute(
            "INSERT INTO alert (kind, address, token, created_at, payload_json, delivered)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                alert.kind.as_str(),
                addr_key(alert.address),
                alert.token.map(addr_key),
                now_secs() as i64,
                alert.payload_json,
                alert.delivered as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Marca el resultado de una alerta. `outcome` es texto libre a propósito:
    /// el vocabulario se decide con alertas reales delante, no antes.
    ///
    /// Falla si el id no existe, en vez de no hacer nada en silencio.
    pub fn set_alert_outcome(
        &self,
        id: i64,
        outcome: &str,
        note: Option<&str>,
    ) -> anyhow::Result<()> {
        let n = self.conn.execute(
            "UPDATE alert SET outcome = ?2, outcome_note = ?3, outcome_at = ?4 WHERE id = ?1",
            params![id, outcome, note, now_secs() as i64],
        )?;
        if n == 0 {
            anyhow::bail!("no existe ninguna alerta con id {id}");
        }
        Ok(())
    }

    /// Últimas alertas, más recientes primero. `only_pending` deja solo las
    /// que siguen sin resultado, que son las que hay que cerrar.
    pub fn recent_alerts(&self, limit: usize, only_pending: bool) -> anyhow::Result<Vec<AlertRow>> {
        let sql = if only_pending {
            "SELECT id, kind, address, token, created_at, payload_json, outcome, outcome_note
             FROM alert WHERE outcome IS NULL ORDER BY id DESC LIMIT ?1"
        } else {
            "SELECT id, kind, address, token, created_at, payload_json, outcome, outcome_note
             FROM alert ORDER BY id DESC LIMIT ?1"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(AlertRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                address: r.get(2)?,
                token: r.get(3)?,
                created_at: r.get::<_, i64>(4)? as u64,
                payload_json: r.get(5)?,
                outcome: r.get(6)?,
                outcome_note: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

/// Tipos de alerta que el programa sabe emitir.
///
/// Hoy hay **uno solo**: la señal 2 del diseño. La señal 1 (financiación
/// previa) está desmontada por los datos del 2026-09-18 y no se le reserva
/// sitio aquí — cuando aparezca una señal de anticipación que sí funcione,
/// será una variante nueva de este enum y una cadena nueva en la columna, sin
/// tocar el schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    /// `TokenLaunched` cuyo deployer está en la watchlist. Accionable: lleva
    /// dirección de token.
    LaunchConfirmed,
}

impl AlertKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AlertKind::LaunchConfirmed => "launch_confirmed",
        }
    }
}

/// Alerta a registrar. `payload_json` lleva lo específico del tipo (pairToken,
/// graduationThreshold, bloque…), que por eso no son columnas.
#[derive(Debug, Clone)]
pub struct NewAlert {
    pub kind: AlertKind,
    /// La wallet vigilada a la que se refiere la alerta.
    pub address: Address,
    /// Token implicado, cuando el tipo de alerta lo tiene.
    pub token: Option<Address>,
    pub payload_json: String,
    /// Si ya se entregó al sink (consola hoy, Telegram más adelante).
    pub delivered: bool,
}

#[derive(Debug, Clone)]
pub struct AlertRow {
    pub id: i64,
    pub kind: String,
    pub address: String,
    pub token: Option<String>,
    pub created_at: u64,
    pub payload_json: String,
    pub outcome: Option<String>,
    pub outcome_note: Option<String>,
}

/// Qué cubre el snapshot cacheado de un operador.
#[derive(Debug, Clone)]
pub struct CacheInfo {
    pub kind_label: String,
    pub history_from_block: u64,
    pub history_to_block: u64,
    pub cached_at: u64,
    pub launches: usize,
    pub fundings: usize,
}

/// `(kind, detalle)` de una clasificación, para guardarla sin perder el
/// motivo. El motivo importa: `NotEvaluated` no es lo mismo que "es ingreso",
/// y esa distinción se perdería guardando solo "income".
pub(crate) fn encode_funding_kind(kind: &FundingKind) -> (&'static str, Option<String>) {
    match kind {
        FundingKind::Startup(Confidence::High) => ("startup_high", None),
        FundingKind::Startup(Confidence::Low(LowReason::AssetNotSpendable)) => {
            ("startup_low", Some("asset_not_spendable".into()))
        }
        FundingKind::Startup(Confidence::Low(LowReason::SenderLooksLikeInfrastructure)) => {
            ("startup_low", Some("sender_infrastructure".into()))
        }
        FundingKind::Income(IncomeReason::NativeInternal) => ("income", Some("native_internal".into())),
        FundingKind::Income(IncomeReason::SenderIsContract) => {
            ("income", Some("sender_is_contract".into()))
        }
        FundingKind::Income(IncomeReason::SelfInitiated) => ("income", Some("self_initiated".into())),
        FundingKind::Income(IncomeReason::NotEvaluated(msg)) => {
            ("not_evaluated", Some(msg.clone()))
        }
    }
}

/// Las direcciones se guardan en minúsculas con `0x`: la clave primaria es
/// textual y `0xAb…` y `0xab…` son la misma dirección.
fn addr_key(a: Address) -> String {
    format!("{a:#x}")
}

fn parse_addr(s: &str) -> anyhow::Result<Address> {
    s.parse::<Address>()
        .map_err(|e| anyhow::anyhow!("dirección corrupta en la base ({s}): {e}"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

const SCHEMA_V1: &str = r#"
CREATE TABLE operator (
    address            TEXT PRIMARY KEY,
    label              TEXT,
    kind               TEXT NOT NULL,
    history_from_block INTEGER NOT NULL,
    history_to_block   INTEGER NOT NULL,
    cached_at          INTEGER NOT NULL
);

CREATE TABLE past_launch (
    address              TEXT NOT NULL REFERENCES operator(address) ON DELETE CASCADE,
    token                TEXT NOT NULL,
    curve                TEXT NOT NULL,
    pair_token           TEXT NOT NULL,
    graduation_threshold TEXT NOT NULL,
    block                INTEGER NOT NULL,
    ts                   INTEGER NOT NULL,
    tx_hash              TEXT NOT NULL,
    PRIMARY KEY (address, token)
);
CREATE INDEX idx_past_launch_block ON past_launch(address, block);

CREATE TABLE funding (
    id                     INTEGER PRIMARY KEY,
    address                TEXT NOT NULL REFERENCES operator(address) ON DELETE CASCADE,
    from_addr              TEXT,
    asset_kind             TEXT NOT NULL,
    asset_token            TEXT,
    asset_decimals         INTEGER,
    amount_raw             TEXT NOT NULL,
    amount                 REAL NOT NULL,
    block                  INTEGER NOT NULL,
    ts                     INTEGER NOT NULL,
    tx_hash                TEXT,
    kind                   TEXT NOT NULL,
    kind_detail            TEXT,
    sender_nonce           INTEGER,
    delay_to_next_launch   INTEGER,
    precedes_first_launch  INTEGER NOT NULL
);
CREATE INDEX idx_funding_addr_block ON funding(address, block);

CREATE TABLE alert (
    id           INTEGER PRIMARY KEY,
    kind         TEXT NOT NULL,
    address      TEXT NOT NULL,
    token        TEXT,
    created_at   INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    delivered    INTEGER NOT NULL DEFAULT 0,
    outcome      TEXT,
    outcome_note TEXT,
    outcome_at   INTEGER
);
CREATE INDEX idx_alert_pending ON alert(outcome, id);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::operator_tracker::{DeployerKind, Funding};

    fn addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn profile(launches: Vec<PastLaunch>) -> OperatorProfile {
        OperatorProfile {
            address: addr(1),
            label: Some("deployer de prueba".into()),
            kind: DeployerKind::Wallet,
            launches,
            history_from_block: 8_991_118,
            history_to_block: 65_000_000,
        }
    }

    fn launch(block: u64, token: u8) -> PastLaunch {
        PastLaunch {
            token: addr(token),
            curve: addr(token + 100),
            pair_token: Address::ZERO,
            graduation_threshold: U256::from(4_200_000_000_000_000_000u128),
            block,
            timestamp: 1_700_000_000 + block,
            tx_hash: B256::repeat_byte(token),
        }
    }

    fn classified(kind: FundingKind) -> ClassifiedFunding {
        ClassifiedFunding {
            funding: Funding {
                asset: FundingAsset::Native,
                from: Some(addr(9)),
                amount_raw: U256::from(9_848_000_000_000_000u128),
                amount: 0.009848,
                block: 64_386_842,
                timestamp: 1_700_000_100,
                tx_hash: Some(B256::repeat_byte(7)),
            },
            kind,
            sender_nonce: Some(325_161),
            delay_to_next_launch: Some(15_300),
            precedes_first_launch: true,
        }
    }

    #[test]
    fn el_perfil_va_y_vuelve_igual() {
        let mut db = Db::open_in_memory().unwrap();
        let p = profile(vec![launch(10, 2), launch(20, 3)]);
        db.save_operator_profile(&p, Some(&[classified(FundingKind::Startup(Confidence::High))]))
            .unwrap();

        let back = db.load_operator_launches(p.address).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].block, 10);
        assert_eq!(back[1].token, addr(3));
        assert_eq!(back[0].graduation_threshold, p.launches[0].graduation_threshold);

        let info = db.operator_cache_info(p.address).unwrap().unwrap();
        assert_eq!(info.launches, 2);
        assert_eq!(info.fundings, 1);
        assert_eq!(info.history_to_block, 65_000_000);
    }

    #[test]
    fn regrabar_reemplaza_el_snapshot_en_vez_de_acumular() {
        // Dos backfills del mismo operador no deben dar una lista cuya
        // cobertura real ya no sepa nadie.
        let mut db = Db::open_in_memory().unwrap();
        db.save_operator_profile(&profile(vec![launch(10, 2), launch(20, 3)]), None).unwrap();
        db.save_operator_profile(&profile(vec![launch(10, 2)]), None).unwrap();
        assert_eq!(db.load_operator_launches(addr(1)).unwrap().len(), 1);
    }

    #[test]
    fn guardar_solo_el_historial_no_borra_la_financiacion_cacheada() {
        // `cargo run -- operator` no calcula financiación: no debe llevarse
        // por delante lo que cacheó un `funding` anterior.
        let mut db = Db::open_in_memory().unwrap();
        let p = profile(vec![launch(10, 2)]);
        db.save_operator_profile(&p, Some(&[classified(FundingKind::Startup(Confidence::High))]))
            .unwrap();
        db.save_operator_profile(&p, None).unwrap();
        assert_eq!(db.operator_cache_info(p.address).unwrap().unwrap().fundings, 1);
    }

    #[test]
    fn el_motivo_de_una_entrada_no_evaluada_sobrevive_al_guardado() {
        // `NotEvaluated` no es "es ingreso": si al guardar se colapsara en
        // "income" se perdería justo la distinción que el módulo cuida.
        let (kind, detail) = encode_funding_kind(&FundingKind::Income(IncomeReason::NotEvaluated(
            "la tx no trae campo `from`".into(),
        )));
        assert_eq!(kind, "not_evaluated");
        assert_eq!(detail.as_deref(), Some("la tx no trae campo `from`"));
        let (kind, _) = encode_funding_kind(&FundingKind::Income(IncomeReason::SenderIsContract));
        assert_eq!(kind, "income");
    }

    #[test]
    fn el_ciclo_de_una_alerta_va_de_pendiente_a_cerrada() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .record_alert(&NewAlert {
                kind: AlertKind::LaunchConfirmed,
                address: addr(1),
                token: Some(addr(2)),
                payload_json: r#"{"pair_token":"0x0"}"#.into(),
                delivered: true,
            })
            .unwrap();

        assert_eq!(db.recent_alerts(10, true).unwrap().len(), 1);
        db.set_alert_outcome(id, "launched", Some("entré y salí en verde")).unwrap();
        assert!(db.recent_alerts(10, true).unwrap().is_empty());
        let all = db.recent_alerts(10, false).unwrap();
        assert_eq!(all[0].outcome.as_deref(), Some("launched"));
        assert_eq!(all[0].kind, "launch_confirmed");
    }

    #[test]
    fn marcar_una_alerta_inexistente_falla_en_vez_de_no_hacer_nada() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.set_alert_outcome(404, "launched", None).is_err());
    }

    #[test]
    fn reabrir_la_base_no_vuelve_a_migrar_ni_pierde_datos() {
        let dir = std::env::temp_dir().join(format!("marxi-db-test-{}", std::process::id()));
        let path = dir.join("test.sqlite3");
        let path_s = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);
        {
            let mut db = Db::open(&path_s).unwrap();
            db.save_operator_profile(&profile(vec![launch(10, 2)]), None).unwrap();
        }
        {
            let db = Db::open(&path_s).unwrap();
            assert_eq!(db.load_operator_launches(addr(1)).unwrap().len(), 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
