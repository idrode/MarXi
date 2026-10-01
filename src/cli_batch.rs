//! `cargo run -- batch-funding <fichero> [--out <prefijo>] [--lookback-blocks N]`
//!
//! Lo que el 2026-09-27 se hacía a mano token a token, para un lote entero:
//! creador real (también si lanzó por intermediario u otro launchpad, ver
//! `operator_tracker::creator`), su nonce, sus entradas de fondos **anteriores
//! al lanzamiento** clasificadas con el criterio de `baseline.rs`, y el aviso de
//! zona gris. Pensado para comparar un lote de éxito (runners) contra uno de
//! control.
//!
//! Salida:
//! - `<prefijo>.csv` — una fila por token, con su financiación principal (la
//!   entrada clasificada como financiación más cercana antes del lanzamiento).
//! - `<prefijo>_entradas.csv` — **todas** las entradas previas de cada creador,
//!   ingreso incluido, para poder rehacer el análisis sin volver a la chain.
//!
//! Un token que falla no aborta el lote: sale como fila `error` con el motivo.
//! Las filas se escriben y vuelcan al disco token a token, así que un corte a
//! mitad de lote conserva lo ya medido.

use crate::chain::ChainProvider;
use crate::cli::{fmt_amount, fmt_duration, fmt_time};
use crate::config::AppConfig;
use crate::data::db::encode_funding_kind;
use crate::data::operator_tracker::baseline::{in_grey_zone, INFRA_NONCE_THRESHOLD};
use crate::data::operator_tracker::creator::{resolve_token_creator, TokenCreator};
use crate::data::operator_tracker::graduation::{graduation_buyers, GraduationBuyers};
use crate::data::operator_tracker::funding::{
    backfill_erc20_fundings, fill_timestamps, find_native_fundings,
};
use crate::data::operator_tracker::{
    classify_fundings, ClassifiedFunding, FundingAsset, OperatorProfile, PastLaunch,
};
use crate::data::token_lookup::LookupAddresses;
use alloy::primitives::Address;
use alloy::providers::Provider;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

/// Ventana por defecto antes del lanzamiento: ~28 h al ritmo medido el
/// 2026-09-27 (~10 bloques/s). En los tres runners medidos a mano la
/// financiación estaba a 11-26 min; la ventana es holgada a propósito, y la
/// columna `nonce_al_inicio_ventana` dice si la wallet ya existía antes.
pub const DEFAULT_LOOKBACK_BLOCKS: u64 = 1_000_000;

/// Financiadores ya medidos a mano, para decir si reaparecen. Datos de
/// CLAUDE.md ("Separar financiación…", "Las EOA financiadoras…" y
/// "Financiación de creadores de runners de Phanes"), no una lista de
/// confianza: casi todos son infraestructura.
const KNOWN_FUNDERS: &[(&str, &str)] = &[
    ("0xf70da97812cb96acdf810712aa562db8dfa3dbef", "infra; financió al caso A (09-18) y al runner 0xc4efd8ac (09-27)"),
    ("0x88d25c861938a91af4ad57ad964a8fcc6c6351d3", "zona gris; financió al runner 0xcc302c3b (09-27)"),
    ("0x0ec4e45f9ce3020024c0418a74cd31fbc487038f", "infra; financió al runner 0xa7dea7ab (09-27)"),
    ("0x059df16bdcf8a6bd20bb4814518ff6a2df3c7b11", "infra; financió al caso B (09-18)"),
    ("0x56c262027e0de4aea31d2489529cb25d23e58a8b", "infra; financió al caso B (09-18)"),
    // Marcador de vigilancia, no financiador confirmado: está para que salte
    // solo si reaparece. Si resulta ser ruido, quitar esta línea.
    ("0xa5df4d576a72ddbbb4296614aaf58ed6696ebe61", "VIGILANCIA (no confirmado); zona gris; financió al runner 0xe142304c (09-28)"),
];

fn known_funder(a: Address) -> Option<&'static str> {
    let s = format!("{a:#x}");
    KNOWN_FUNDERS.iter().find(|(k, _)| *k == s).map(|(_, v)| *v)
}

/// Marca de lectura de una entrada: por el nonce del remitente, o
/// `entrega_interna` si llegó por llamada interna (no hay remitente cuyo nonce
/// leer, y "nonce_desconocido" sugeriría un fallo que no hubo).
fn funding_flag(c: &ClassifiedFunding) -> &'static str {
    if c.funding.from.is_none() {
        "entrega_interna"
    } else {
        sender_flag(c.sender_nonce)
    }
}

/// Marca de lectura por el nonce del remitente (no cambia la clasificación).
fn sender_flag(nonce: Option<u64>) -> &'static str {
    match nonce {
        Some(n) if n >= INFRA_NONCE_THRESHOLD => "infra",
        Some(n) if in_grey_zone(n) => "ZONA_GRIS",
        Some(_) => "",
        None => "nonce_desconocido",
    }
}

/// Resultado de un token medido sin error.
struct Measured {
    creator: TokenCreator,
    launch_ts: u64,
    creator_nonce: u64,
    nonce_at_window_start: Option<u64>,
    /// Saldo nativo (ETH) al inicio de la ventana. Si es > 0, la wallet ya
    /// estaba financiada antes y la ventana no ve su financiación: el nonce no
    /// lo dice, porque recibir ETH no lo sube (caso medido: JACKET, nonce 0
    /// y saldo previo).
    balance_at_window_start: Option<f64>,
    window_from: u64,
    classified: Vec<ClassifiedFunding>,
    native_truncated: bool,
    native_internal: usize,
    /// Solo Pons V2 y solo si graduó: quién compró la curva hasta graduar.
    graduation: Option<GraduationBuyers>,
    graduation_ts: u64,
}

impl Measured {
    /// La financiación de arranque más cercana antes del lanzamiento.
    fn main_funding(&self) -> Option<&ClassifiedFunding> {
        self.classified.iter().filter(|c| c.is_startup()).max_by_key(|c| c.funding.block)
    }

    /// `si` / `no` / vacío (no graduó) / `n/a` (otro launchpad: no hay curva).
    fn auto_graduated(&self) -> &'static str {
        if self.creator.curve.is_none() {
            return "n/a";
        }
        match &self.graduation {
            None => "",
            Some(g) if g.is_self_graduated() => "si",
            Some(_) => "no",
        }
    }

    /// Etiqueta de éxito para el resumen: separa los dos tipos de graduación.
    fn outcome(&self) -> &'static str {
        match self.auto_graduated() {
            "si" => "graduó: AUTO-GRADUADO (el creador compró la curva)",
            "no" => "graduó por compras de terceros",
            "n/a" => "otro launchpad (sin curva ni graduación)",
            _ => "no graduó (todavía)",
        }
    }

    fn funded_before_window(&self) -> bool {
        self.balance_at_window_start.is_some_and(|b| b > 0.0)
    }

    /// Por qué no hay financiación principal, en una frase.
    fn no_funding_reason(&self) -> String {
        match (self.classified.len(), self.funded_before_window()) {
            (0, true) => format!(
                "sin entradas en la ventana; ya tenía {} ETH al empezar: financiada ANTES (amplía --lookback-blocks)",
                fmt_amount(self.balance_at_window_start.unwrap_or(0.0))
            ),
            (0, false) => "sin entradas en la ventana y sin saldo previo (¿entrada interna no vista?)".into(),
            (n, _) => format!("{n} entradas en la ventana, ninguna es financiación externa"),
        }
    }
}

pub async fn run_batch_funding_cli(
    cfg: &AppConfig,
    input: &str,
    out_prefix: Option<&str>,
    lookback_blocks: u64,
) -> anyhow::Result<()> {
    let tokens = read_token_list(input)?;
    anyhow::ensure!(!tokens.is_empty(), "{input}: no hay ninguna dirección (una por línea, # para comentarios)");

    let prefix = match out_prefix {
        Some(p) => p.to_string(),
        None => std::path::Path::new(input).with_extension("").to_string_lossy().into_owned(),
    };
    let (main_path, entries_path) = (format!("{prefix}.csv"), format!("{prefix}_entradas.csv"));
    for p in [&main_path, &entries_path] {
        anyhow::ensure!(
            std::path::Path::new(p) != std::path::Path::new(input),
            "la salida {p} pisaría el fichero de entrada: usa --out <prefijo>"
        );
    }

    let provider = ChainProvider::connect(&cfg.chain).await?;
    let addrs = LookupAddresses::from_config(cfg)?;
    let chunk = cfg.indexer.backfill_max_blocks_per_request;

    // Reanudable: si la salida ya existe, se conservan los tokens con fila `ok`
    // y se mide el resto (ver `prepare_resume`).
    let done = prepare_resume(&main_path, &entries_path, &tokens)?;
    let mut main_csv = std::fs::OpenOptions::new().append(true).open(&main_path)?;
    let mut entries_csv = std::fs::OpenOptions::new().append(true).open(&entries_path)?;

    println!(
        "batch-funding: {} tokens de {input}; ventana {lookback_blocks} bloques antes de cada lanzamiento",
        tokens.len()
    );
    if !done.is_empty() {
        println!(
            "REANUDANDO: {} tokens ya medidos en {main_path} se conservan y se saltan; quedan {}",
            done.len(),
            tokens.len() - done.len()
        );
    }
    let started = std::time::Instant::now();
    let mut results: Vec<(String, Result<Measured, String>)> = Vec::new();
    for (i, raw) in tokens.iter().enumerate() {
        if done.contains(&raw.to_lowercase()) {
            continue;
        }
        println!("[{}/{}] {raw} ...", i + 1, tokens.len());
        let res = match raw.parse::<Address>() {
            Err(e) => Err(format!("no es una dirección EVM válida: {e}")),
            Ok(token) => measure_token(&provider, addrs.factory, token, chunk, lookback_blocks)
                .await
                .map_err(|e| format!("{e:#}")),
        };
        // Orden a propósito: primero las entradas (y al disco), después la fila
        // principal. La fila principal es la marca de "token terminado"; si el
        // proceso muere entre medias, las entradas huérfanas se descartan al
        // reanudar.
        match &res {
            Ok(m) => {
                for c in &m.classified {
                    write_entry_row(&mut entries_csv, m, c)?;
                }
                entries_csv.flush()?;
                write_main_row(&mut main_csv, raw, Ok(m))?;
                println!(
                    "    {} · creador {} · nonce {} · {} entradas previas",
                    m.creator.via.code(),
                    m.creator.creator,
                    m.creator_nonce,
                    m.classified.len()
                );
            }
            Err(e) => {
                write_main_row(&mut main_csv, raw, Err(e))?;
                println!("    ERROR: {e}");
            }
        }
        main_csv.flush()?;
        entries_csv.flush()?;
        results.push((raw.clone(), res));
    }

    print_summary(&results);
    if !done.is_empty() {
        println!(
            "\nOJO: el resumen de arriba cubre solo los {} tokens medidos en esta ejecución; \
             los {} reanudados están en {main_path}.",
            results.len(),
            done.len()
        );
    }
    println!(
        "\n{:.0} s en total. Escrito: {main_path} (una fila por token) y {entries_path} (todas las entradas previas).",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Deja los dos CSV listos para añadir filas y devuelve los tokens (en
/// minúsculas) que ya están terminados.
///
/// - Si `main_path` no existe, crea los dos ficheros con su cabecera.
/// - Si existe, **conserva solo las filas `ok`** del principal; las filas
///   `error` se vuelven a medir (un 429 o un timeout no es un resultado). Una
///   última línea sin `\n` (escritura cortada) se descarta.
/// - Del de entradas conserva solo las de tokens terminados: las de un token
///   que murió a medias, sin fila principal, se descartan.
/// - Se niega si la cabecera no es la de este binario (otra versión del
///   esquema) o si el CSV tiene tokens que no están en la lista de entrada
///   (otro lote con el mismo `--out`).
///
/// La reescritura va a un `.tmp` y se renombra, para no dejar los ficheros a
/// medias si el proceso muere aquí.
fn prepare_resume(main_path: &str, entries_path: &str, tokens: &[String]) -> anyhow::Result<BTreeSet<String>> {
    let main_header = MAIN_HEADER.join(",");
    let entries_header = ENTRIES_HEADER.join(",");
    if !std::path::Path::new(main_path).exists() {
        std::fs::write(main_path, format!("{main_header}\n"))?;
        std::fs::write(entries_path, format!("{entries_header}\n"))?;
        return Ok(BTreeSet::new());
    }

    let wanted: BTreeSet<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
    let main_text = std::fs::read_to_string(main_path)?;
    let mut lines = complete_lines(&main_text);
    let header = lines.next().unwrap_or_default();
    anyhow::ensure!(
        header == main_header,
        "{main_path} existe con otra cabecera (otra versión del esquema): no se puede reanudar; muévelo o usa otro --out"
    );
    let mut done = BTreeSet::new();
    let mut kept = vec![main_header];
    let mut dropped_errors = 0;
    for line in lines {
        let mut cols = line.splitn(3, ',');
        let (tok, estado) = (cols.next().unwrap_or_default().to_lowercase(), cols.next().unwrap_or_default());
        anyhow::ensure!(
            wanted.contains(&tok),
            "{main_path} contiene {tok}, que no está en la lista de entrada: ¿otro lote con el mismo --out?"
        );
        if estado == "ok" {
            done.insert(tok);
            kept.push(line.to_string());
        } else {
            dropped_errors += 1;
        }
    }

    let entries_text = std::fs::read_to_string(entries_path).unwrap_or_default();
    let mut elines = complete_lines(&entries_text);
    let eheader = elines.next().unwrap_or_default();
    anyhow::ensure!(
        eheader.is_empty() || eheader == entries_header,
        "{entries_path} existe con otra cabecera: no se puede reanudar; muévelo o usa otro --out"
    );
    let mut ekept = vec![entries_header];
    let mut orphans = 0;
    for line in elines {
        let tok = line.split(',').next().unwrap_or_default().to_lowercase();
        if done.contains(&tok) {
            ekept.push(line.to_string());
        } else {
            orphans += 1;
        }
    }

    for (path, rows) in [(main_path, &kept), (entries_path, &ekept)] {
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, rows.iter().map(|r| format!("{r}\n")).collect::<String>())?;
        std::fs::rename(&tmp, path)?;
    }
    if dropped_errors > 0 {
        println!("reanudar: {dropped_errors} filas `error` se descartan y se vuelven a medir");
    }
    if orphans > 0 {
        println!("reanudar: {orphans} entradas de un token sin terminar se descartan");
    }
    Ok(done)
}

/// Líneas terminadas en `\n`; una última línea sin terminar (escritura
/// cortada a medias) se descarta.
fn complete_lines(text: &str) -> impl Iterator<Item = &str> {
    let end = text.rfind('\n').map_or(0, |i| i + 1);
    text[..end].lines()
}

/// Una dirección por línea; `#` empieza un comentario; líneas vacías fuera.
/// Los duplicados se quitan avisando, no en silencio.
fn read_token_list(path: &str) -> anyhow::Result<Vec<String>> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("no se pudo leer {path}: {e}"))?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(tok) = line.split('#').next().and_then(|l| l.split_whitespace().next()) else {
            continue;
        };
        if seen.insert(tok.to_lowercase()) {
            out.push(tok.to_string());
        } else {
            println!("aviso: {tok} está repetida en {path}; se mide una vez");
        }
    }
    Ok(out)
}

async fn measure_token(
    provider: &ChainProvider,
    factory: Address,
    token: Address,
    chunk: u64,
    lookback: u64,
) -> anyhow::Result<Measured> {
    let creator = resolve_token_creator(provider, factory, token, chunk).await?;
    let launch_block = creator.launch_block;
    let launch_ts = crate::data::backfill::resolve_block_timestamps(provider, &[launch_block])
        .await?
        .get(&launch_block)
        .copied()
        .unwrap_or(0);
    let wallet = creator.creator;
    let creator_nonce = provider.http().get_transaction_count(wallet).await?;

    // Solo lo anterior al lanzamiento: el bloque del lanzamiento trae ya el
    // reparto interno de la propia curva, que es ingreso, no financiación.
    let window_from = launch_block.saturating_sub(lookback).max(1);
    let window_to = launch_block.saturating_sub(1);
    let nonce_at_window_start = provider
        .http()
        .get_transaction_count(wallet)
        .block_id(window_from.into())
        .await
        .map_err(|e| tracing::warn!(%wallet, "nonce histórico no disponible: {e}"))
        .ok();
    let balance_at_window_start = provider
        .http()
        .get_balance(wallet)
        .block_id(window_from.into())
        .await
        .map_err(|e| tracing::warn!(%wallet, "saldo histórico no disponible: {e}"))
        .ok()
        .map(|b| crate::data::token_lookup::scaled(b, 18));

    let mut fundings = backfill_erc20_fundings(provider, wallet, window_from, window_to, chunk).await?;
    let scan = find_native_fundings(provider, wallet, window_from, window_to).await?;
    fundings.extend(scan.fundings);
    fill_timestamps(provider, &mut fundings).await?;
    fundings.sort_by_key(|f| f.block);

    // Perfil de un solo lanzamiento, el de este token: así la clasificación
    // mide el retardo contra él y sabe qué pairToken es gastable. En otro
    // launchpad el par no se conoce: `Address::ZERO` = solo ETH gastable, y
    // todo ERC-20 queda en confianza baja (se dice en el CSV con `par`).
    let profile = OperatorProfile {
        address: wallet,
        label: None,
        kind: creator.creator_kind,
        launches: vec![PastLaunch {
            token,
            curve: creator.curve.unwrap_or(Address::ZERO),
            pair_token: creator.pair_token.unwrap_or(Address::ZERO),
            graduation_threshold: creator.graduation_threshold.unwrap_or_default(),
            block: launch_block,
            timestamp: launch_ts,
            tx_hash: creator.launch_tx,
        }],
        history_from_block: window_from,
        history_to_block: window_to,
    };
    let classified = classify_fundings(provider, &profile, fundings).await?;

    let graduation = match creator.curve {
        Some(curve) => {
            graduation_buyers(provider, factory, token, curve, wallet, launch_block, chunk).await?
        }
        None => None,
    };
    let graduation_ts = match &graduation {
        Some(g) => crate::data::backfill::resolve_block_timestamps(provider, &[g.graduation_block])
            .await?
            .get(&g.graduation_block)
            .copied()
            .unwrap_or(0),
        None => 0,
    };

    Ok(Measured {
        creator,
        launch_ts,
        creator_nonce,
        nonce_at_window_start,
        balance_at_window_start,
        window_from,
        classified,
        native_truncated: scan.truncated,
        native_internal: scan.native_internal,
        graduation,
        graduation_ts,
    })
}

const MAIN_HEADER: &[&str] = &[
    "token", "estado", "error", "via", "via_contrato", "creador", "tipo_creador", "nonce_creador",
    "nonce_al_inicio_ventana", "saldo_eth_al_inicio_ventana", "bloque_lanzamiento", "fecha_lanzamiento_utc", "tx_lanzamiento", "par",
    "ventana_desde_bloque", "entradas_previas", "financiacion_alta", "financiacion_baja", "ingreso",
    "sin_evaluar", "nativo_truncado", "nativo_interno", "fin_remitente", "fin_activo", "fin_importe",
    "fin_bloque", "fin_segundos_antes", "fin_veredicto", "fin_detalle", "fin_nonce_remitente",
    "fin_marca", "fin_conocido", "sin_financiacion_motivo", "graduo", "bloque_graduacion",
    "segundos_hasta_graduar", "compras_hasta_graduar", "receptores_distintos_hasta_graduar",
    "compra_creador_pct", "auto_graduado", "atribucion", "atribucion_dudosa", "ejecutor",
];

const ENTRIES_HEADER: &[&str] = &[
    "token", "creador", "bloque", "fecha_utc", "segundos_antes_lanzamiento", "activo", "importe",
    "remitente", "veredicto", "detalle", "nonce_remitente", "marca", "conocido", "tx",
];

fn write_main_row(w: &mut impl Write, raw: &str, res: Result<&Measured, &String>) -> anyhow::Result<()> {
    let m = match res {
        Err(e) => {
            let mut row = vec![raw.to_string(), "error".into(), e.clone()];
            row.resize(MAIN_HEADER.len(), String::new());
            return write_row(w, &row);
        }
        Ok(m) => m,
    };
    let c = &m.creator;
    let count = |k: &str| m.classified.iter().filter(|x| encode_funding_kind(&x.kind).0 == k).count();
    let mut row = vec![
        raw.to_string(),
        "ok".into(),
        String::new(),
        c.via.code().into(),
        opt_addr(c.via.contract()),
        addr(c.creator),
        c.creator_kind.label(),
        m.creator_nonce.to_string(),
        m.nonce_at_window_start.map(|n| n.to_string()).unwrap_or_default(),
        m.balance_at_window_start.map(|b| b.to_string()).unwrap_or_default(),
        c.launch_block.to_string(),
        fmt_time(m.launch_ts),
        format!("{:#x}", c.launch_tx),
        opt_addr(c.pair_token),
        m.window_from.to_string(),
        m.classified.len().to_string(),
        count("startup_high").to_string(),
        count("startup_low").to_string(),
        count("income").to_string(),
        count("not_evaluated").to_string(),
        m.native_truncated.to_string(),
        m.native_internal.to_string(),
    ];
    match m.main_funding() {
        None => {
            row.resize(MAIN_HEADER.len() - GRADUATION_COLS - ATTRIBUTION_COLS - 1, String::new());
            row.push(m.no_funding_reason());
        }
        Some(f) => {
            let (kind, detail) = encode_funding_kind(&f.kind);
            row.extend([
                sender_cell(&f.funding),
                asset(&f.funding.asset),
                f.funding.amount.to_string(),
                f.funding.block.to_string(),
                secs_before(m, f),
                kind.into(),
                detail.unwrap_or_default(),
                f.sender_nonce.map(|n| n.to_string()).unwrap_or_default(),
                funding_flag(f).into(),
                f.funding.from.and_then(known_funder).unwrap_or_default().into(),
                String::new(),
            ]);
        }
    }
    let g = m.graduation.as_ref();
    row.extend([
        match (m.creator.curve.is_some(), g.is_some()) {
            (false, _) => "n/a".into(),
            (true, true) => "si".into(),
            (true, false) => "no".into(),
        },
        g.map(|g| g.graduation_block.to_string()).unwrap_or_default(),
        g.filter(|_| m.graduation_ts > 0 && m.launch_ts > 0)
            .map(|_| m.graduation_ts.saturating_sub(m.launch_ts).to_string())
            .unwrap_or_default(),
        g.map(|g| g.buys.to_string()).unwrap_or_default(),
        g.map(|g| g.distinct_recipients.to_string()).unwrap_or_default(),
        g.and_then(|g| g.creator_share()).map(|s| format!("{:.1}", 100.0 * s)).unwrap_or_default(),
        m.auto_graduated().into(),
        c.attribution.code().into(),
        if c.attribution.doubtful() { "si" } else { "no" }.into(),
        opt_addr(c.attribution.executor()),
    ]);
    debug_assert_eq!(row.len(), MAIN_HEADER.len());
    write_row(w, &row)
}

/// Columnas de graduación, justo antes de las de atribución en `MAIN_HEADER`.
const GRADUATION_COLS: usize = 7;
/// Columnas de atribución de creador, al final de `MAIN_HEADER`.
const ATTRIBUTION_COLS: usize = 3;

fn write_entry_row(w: &mut impl Write, m: &Measured, c: &ClassifiedFunding) -> anyhow::Result<()> {
    let (kind, detail) = encode_funding_kind(&c.kind);
    let row = vec![
        addr(m.creator.token),
        addr(m.creator.creator),
        c.funding.block.to_string(),
        fmt_time(c.funding.timestamp),
        secs_before(m, c),
        asset(&c.funding.asset),
        c.funding.amount.to_string(),
        sender_cell(&c.funding),
        kind.into(),
        detail.unwrap_or_default(),
        c.sender_nonce.map(|n| n.to_string()).unwrap_or_default(),
        if c.funding.from.is_some() && c.is_startup() { sender_flag(c.sender_nonce).into() } else { String::new() },
        c.funding.from.and_then(known_funder).unwrap_or_default().into(),
        c.funding.tx_hash.map(|h| format!("{h:#x}")).unwrap_or_default(),
    ];
    write_row(w, &row)
}

fn secs_before(m: &Measured, c: &ClassifiedFunding) -> String {
    if m.launch_ts == 0 || c.funding.timestamp == 0 {
        return String::new();
    }
    m.launch_ts.saturating_sub(c.funding.timestamp).to_string()
}

fn write_row(w: &mut impl Write, fields: &[String]) -> anyhow::Result<()> {
    let line: Vec<String> = fields
        .iter()
        .map(|f| {
            if f.contains([',', '"', '\n']) {
                format!("\"{}\"", f.replace('"', "\"\""))
            } else {
                f.clone()
            }
        })
        .collect();
    writeln!(w, "{}", line.join(","))?;
    Ok(())
}

fn addr(a: Address) -> String {
    format!("{a:#x}")
}

/// Remitente de una entrada; en una nativa interna, `via:<contrato>` que la
/// entregó (si se pudo atribuir), para no dejar la columna vacía.
fn sender_cell(f: &crate::data::operator_tracker::Funding) -> String {
    match (f.from, f.via_contract) {
        (Some(a), _) => addr(a),
        (None, Some(v)) => format!("via:{}", addr(v)),
        (None, None) => String::new(),
    }
}

fn opt_addr(a: Option<Address>) -> String {
    a.map(addr).unwrap_or_default()
}

fn asset(a: &FundingAsset) -> String {
    match a {
        FundingAsset::Native => "ETH".into(),
        FundingAsset::Erc20 { token, .. } => addr(*token),
    }
}

fn short(a: Address) -> String {
    let s = addr(a);
    format!("{}…{}", &s[..8], &s[s.len() - 4..])
}

fn print_summary(results: &[(String, Result<Measured, String>)]) {
    println!("\n=== POR TOKEN ===");
    println!(
        "  {:<14} {:<22} {:<14} {:>7}  {:>16} {:<6} {:>8}  {:<14} {}",
        "token", "vía", "creador", "nonce", "financiación", "activo", "antes", "financiador", "veredicto"
    );
    for (raw, res) in results {
        let label = raw.parse::<Address>().map(short).unwrap_or_else(|_| raw.clone());
        let m = match res {
            Err(e) => {
                println!("  {label:<14} ERROR: {e}");
                continue;
            }
            Ok(m) => m,
        };
        let head = format!(
            "  {label:<14} {:<22} {:<14} {:>7}",
            m.creator.via.code(),
            short(m.creator.creator),
            m.creator_nonce
        );
        match m.main_funding() {
            None => println!("{head}  {}", m.no_funding_reason()),
            Some(f) => {
                let (kind, detail) = encode_funding_kind(&f.kind);
                let flag = funding_flag(f);
                let verdict = match (flag, detail) {
                    ("ZONA_GRIS", _) => format!("{kind} → ZONA GRIS: no confiar en la clasificación automática"),
                    (_, Some(d)) => format!("{kind} ({d})"),
                    (_, None) => kind.to_string(),
                };
                let antes = secs_before(m, f).parse::<u64>().map(fmt_duration).unwrap_or_default();
                let asset_label = match &f.funding.asset {
                    FundingAsset::Native => "ETH".to_string(),
                    FundingAsset::Erc20 { token, .. } => short(*token),
                };
                println!(
                    "{head}  {:>16} {:<6} {:>8}  {:<14} {verdict}",
                    fmt_amount(f.funding.amount),
                    asset_label,
                    antes,
                    match (f.funding.from, f.funding.via_contract) {
                        (Some(a), _) => short(a),
                        (None, Some(v)) => format!("vía {}", short(v)),
                        (None, None) => "?".into(),
                    },
                );
            }
        }
        let grad_detail = match &m.graduation {
            Some(g) => format!(
                " · {} compras, creador {}",
                g.buys,
                g.creator_share().map(|s| format!("{:.1} %", 100.0 * s)).unwrap_or_else(|| "?".into())
            ),
            None => String::new(),
        };
        println!("  {:<14} └ {}{grad_detail}", "", m.outcome());
        if m.native_truncated {
            println!("  {:<14} !! la bisección nativa llegó al tope de llamadas: recuento INCOMPLETO", "");
        }
    }

    let ok: Vec<&Measured> = results.iter().filter_map(|(_, r)| r.as_ref().ok()).collect();
    let errors = results.len() - ok.len();

    println!("\n=== RESUMEN DEL LOTE ===");
    println!("  tokens medidos {}  ·  con error {errors}", ok.len());
    let mut via: BTreeMap<&str, usize> = BTreeMap::new();
    for m in &ok {
        *via.entry(m.creator.via.code()).or_default() += 1;
    }
    for (k, n) in &via {
        println!("  vía {k:<24} {n}");
    }
    let mut outcomes: BTreeMap<&str, usize> = BTreeMap::new();
    for m in &ok {
        *outcomes.entry(m.outcome()).or_default() += 1;
    }
    for (k, n) in &outcomes {
        println!("  {k:<52} {n}");
    }
    let mut flags: BTreeMap<&str, usize> = BTreeMap::new();
    for m in &ok {
        let f = match m.main_funding() {
            None if m.funded_before_window() => "financiada antes de la ventana",
            None => "sin financiación externa en la ventana",
            Some(f) => match funding_flag(f) {
                "infra" => "financiador infra (nonce ≥ 100 000)",
                "entrega_interna" => "entrega interna por contrato (bridge/relay)",
                "ZONA_GRIS" => "financiador en ZONA GRIS",
                "nonce_desconocido" => "financiador con nonce desconocido",
                _ => "financiador wallet normal",
            },
        };
        *flags.entry(f).or_default() += 1;
    }
    for (k, n) in &flags {
        println!("  {k:<40} {n}");
    }

    // Financiadores que se repiten entre creadores del lote: la señal que se
    // busca. Solo entradas clasificadas como financiación, con remitente.
    let mut by_funder: BTreeMap<Address, BTreeSet<Address>> = BTreeMap::new();
    for m in &ok {
        for c in m.classified.iter().filter(|c| c.is_startup()) {
            if let Some(from) = c.funding.from {
                by_funder.entry(from).or_default().insert(m.creator.creator);
            }
        }
    }
    let repeated: Vec<_> = by_funder.iter().filter(|(_, s)| s.len() > 1).collect();
    println!("\n=== FINANCIADORES REPETIDOS ENTRE CREADORES DEL LOTE ===");
    if repeated.is_empty() {
        println!("  ninguno: cada creador tiene financiadores distintos.");
    }
    for (f, creators) in repeated {
        let nonce = ok
            .iter()
            .flat_map(|m| m.classified.iter())
            .find(|c| c.funding.from == Some(*f))
            .and_then(|c| c.sender_nonce);
        println!(
            "  {f}  → {} creadores · nonce {} {}{}",
            creators.len(),
            nonce.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
            sender_flag(nonce),
            known_funder(*f).map(|k| format!(" · ya visto: {k}")).unwrap_or_default()
        );
    }

    let known: BTreeSet<(Address, &str)> = by_funder
        .keys()
        .filter_map(|f| known_funder(*f).map(|k| (*f, k)))
        .collect();
    println!("\n=== FINANCIADORES YA VISTOS EN SESIONES ANTERIORES ===");
    if known.is_empty() {
        println!("  ninguno.");
    }
    for (f, k) in known {
        println!("  {f}  {k}  (en {} creadores de este lote)", by_funder[&f].len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zona_gris_tiene_los_limites_decididos() {
        // Decisión del 2026-09-27: (10 929, 100 000) es zona gris; los extremos no.
        assert_eq!(sender_flag(Some(10_929)), "");
        assert_eq!(sender_flag(Some(10_930)), "ZONA_GRIS");
        assert_eq!(sender_flag(Some(71_182)), "ZONA_GRIS"); // 0x88d25c86…, el caso que la motivó
        assert_eq!(sender_flag(Some(99_999)), "ZONA_GRIS");
        assert_eq!(sender_flag(Some(100_000)), "infra");
        assert_eq!(sender_flag(None), "nonce_desconocido");
    }

    #[test]
    fn la_lista_quita_comentarios_vacias_y_duplicados() {
        let path = std::env::temp_dir().join(format!("marxi_batch_{}.txt", std::process::id()));
        std::fs::write(&path, "# cabecera\n\n0xAb  # comentario\n0xab\n  0xcd\n").unwrap();
        let v = read_token_list(path.to_str().unwrap()).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(v, vec!["0xAb".to_string(), "0xcd".to_string()]);
    }

    #[test]
    fn reanudar_conserva_lo_terminado_y_descarta_lo_cortado() {
        let dir = std::env::temp_dir().join(format!("marxi_resume_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (main, entries) = (dir.join("l.csv"), dir.join("l_entradas.csv"));
        let (main, entries) = (main.to_str().unwrap(), entries.to_str().unwrap());
        let tokens: Vec<String> = ["0xAA", "0xbb", "0xcc"].iter().map(|s| s.to_string()).collect();
        // 0xaa terminado; 0xbb con fila `error`; 0xcc murió a medias (entradas
        // sin fila principal, y la última línea cortada sin `\n`).
        std::fs::write(
            main,
            format!("{}\n0xAA,ok,,x\n0xbb,error,timeout\n0xcc,o", MAIN_HEADER.join(",")),
        )
        .unwrap();
        std::fs::write(
            entries,
            format!("{}\n0xaa,c,1\n0xcc,c,2\n0xcc,c,", ENTRIES_HEADER.join(",")),
        )
        .unwrap();

        let done = prepare_resume(main, entries, &tokens).unwrap();
        assert_eq!(done, BTreeSet::from(["0xaa".to_string()]));
        assert_eq!(
            std::fs::read_to_string(main).unwrap(),
            format!("{}\n0xAA,ok,,x\n", MAIN_HEADER.join(","))
        );
        assert_eq!(
            std::fs::read_to_string(entries).unwrap(),
            format!("{}\n0xaa,c,1\n", ENTRIES_HEADER.join(","))
        );

        // Un token que no está en la lista: se niega (otro lote, mismo --out).
        let otros = vec!["0xbb".to_string()];
        assert!(prepare_resume(main, entries, &otros).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn el_csv_escapa_comas_y_comillas() {
        let mut out = Vec::new();
        write_row(&mut out, &["a".into(), "b,c".into(), "d\"e".into()]).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a,\"b,c\",\"d\"\"e\"\n");
    }
}
