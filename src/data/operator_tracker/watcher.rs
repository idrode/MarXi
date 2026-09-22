//! Vigilancia en vivo de la **señal 2**: un `TokenLaunched` cuyo deployer
//! está en `watchlist.toml`.
//!
//! Es la única señal que sobrevive del diseño original de vigilancia. La
//! señal 1 (financiación previa) quedó desmontada por los datos del
//! 2026-09-18 —el operador conocido se autofinancia con sus ingresos y sus
//! financiadores son relays compartidos, no wallets madre— y el paso 5 se
//! descartó con ella. Esta no depende de nada de aquello: es accionable
//! (lleva dirección de token) y el filtrado lo hace el nodo.
//!
//! Disciplina:
//! - **el nodo filtra**: `deployer` es indexed (`topics[3]`), y una sola
//!   suscripción con la lista entera de la watchlist en OR cubre a todos;
//! - **nada se descarta en silencio**: un log que no decodifica es un error
//!   que se ve, no un `continue`;
//! - **la alerta se muestra aunque la DB falle**: perder el aviso en pantalla
//!   porque no se pudo escribir en disco sería el peor intercambio posible.

use crate::app::AppEvent;
use crate::chain::ChainProvider;
use crate::data::abi::{decode_token_launched, token_launched_by_deployers_filter};
use crate::data::db::{AlertKind, Db, NewAlert};
use crate::data::operator_tracker::watchlist::Watchlist;
use alloy::primitives::{Address, B256};
use alloy::providers::Provider;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

/// Cada cuánto se sondea cuando no hay WebSocket disponible.
///
/// El `rpc_provider = "public"` no ofrece WS (ver `ChainProvider::ws`), así
/// que sin esta vía la vigilancia sencillamente no arrancaría en esa
/// configuración. 10 s es un compromiso: el launchpad produce del orden de un
/// lanzamiento cada pocos segundos en total, pero de una wallet concreta la
/// cadencia medida es de ~2 min, así que el retraso añadido es irrelevante
/// frente a la ventana de uso.
const POLL_SECS: u64 = 10;

/// Espera entre reintentos cuando la suscripción se cae, con tope.
const RECONNECT_BASE_SECS: u64 = 2;
const RECONNECT_MAX_SECS: u64 = 60;

/// Cuántas claves de log recordadas para no alertar dos veces del mismo
/// lanzamiento. Acotado a propósito: sin tope sería una fuga de memoria en
/// una sesión larga, que es exactamente la deuda nº3 de `recent_launches`.
const SEEN_CAPACITY: usize = 4096;

/// Por qué vía está mirando la chain el vigilante. Se muestra en la TUI: si
/// se degradó a sondeo, el usuario tiene que poder verlo sin mirar logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchTransport {
    WebSocket,
    Polling,
}

impl WatchTransport {
    pub fn label(self) -> &'static str {
        match self {
            WatchTransport::WebSocket => "WebSocket",
            WatchTransport::Polling => "sondeo eth_getLogs",
        }
    }
}

/// Lo que hay que saber de una wallet vigilada al emitir la alerta.
struct Targets {
    addresses: Vec<Address>,
    labels: HashMap<Address, String>,
}

impl Targets {
    fn from_watchlist(list: &Watchlist) -> anyhow::Result<Self> {
        let mut addresses = Vec::new();
        let mut labels = HashMap::new();
        for op in &list.operators {
            let a = op.parsed_address()?;
            if let Some(l) = &op.label {
                labels.insert(a, l.clone());
            }
            addresses.push(a);
        }
        Ok(Self { addresses, labels })
    }
}

/// Ventana de claves de log ya alertadas, con tope.
struct Seen {
    set: HashSet<(B256, u64)>,
    order: VecDeque<(B256, u64)>,
}

impl Seen {
    fn new() -> Self {
        Self { set: HashSet::new(), order: VecDeque::new() }
    }

    /// `true` si es la primera vez que se ve esta clave.
    fn insert(&mut self, key: (B256, u64)) -> bool {
        if !self.set.insert(key) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > SEEN_CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

/// Arranca la vigilancia en su propia tarea. Devuelve el `JoinHandle` para
/// poder pararla (la TUI lo usa al recargar la watchlist).
///
/// No arranca con la lista vacía: un filtro sin `topics[3]` traería el
/// launchpad entero, que es justo lo contrario de lo que se pide.
#[allow(clippy::too_many_arguments)]
pub fn spawn_launch_watcher(
    provider: Arc<ChainProvider>,
    factory: Address,
    watchlist: Watchlist,
    db_path: String,
    tx: Sender<AppEvent>,
    chunk_blocks: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let targets = match Targets::from_watchlist(&watchlist) {
            Ok(t) => t,
            Err(e) => {
                let _ = tx
                    .send(AppEvent::OperatorWatchFailed { message: format!("{e}") })
                    .await;
                return;
            }
        };
        if targets.addresses.is_empty() {
            let _ = tx
                .send(AppEvent::OperatorWatchFailed {
                    message: "la watchlist está vacía: no hay nada que vigilar (cargo run -- watch <addr>)"
                        .to_string(),
                })
                .await;
            return;
        }

        if let Err(e) = watch_loop(&provider, factory, &targets, &db_path, &tx, chunk_blocks).await {
            tracing::error!("la vigilancia de la señal 2 se detiene: {e}");
            let _ = tx
                .send(AppEvent::OperatorWatchFailed { message: format!("{e}") })
                .await;
        }
    })
}

/// Bucle principal. Solo devuelve error si la vigilancia no puede continuar
/// de ninguna manera; las caídas de la suscripción se reintentan aquí dentro.
async fn watch_loop(
    provider: &ChainProvider,
    factory: Address,
    targets: &Targets,
    db_path: &str,
    tx: &Sender<AppEvent>,
    chunk_blocks: u64,
) -> anyhow::Result<()> {
    let filter = token_launched_by_deployers_filter(factory, &targets.addresses);
    let mut seen = Seen::new();

    // Desde dónde se recupera tras una caída. Se arranca en el bloque actual:
    // lo anterior es historial, y para eso está `cargo run -- operator`.
    let mut last_seen_block = provider.http().get_block_number().await?;

    let transport = if provider.has_ws() { WatchTransport::WebSocket } else { WatchTransport::Polling };
    let _ = tx
        .send(AppEvent::OperatorWatchStarted {
            watched: targets.addresses.len(),
            transport,
            from_block: last_seen_block,
        })
        .await;

    let mut backoff = RECONNECT_BASE_SECS;

    loop {
        // Recuperación: todo lo ocurrido entre `last_seen_block` y ahora.
        // Se hace al arrancar y después de cada caída, que es justo cuando
        // hay un hueco. El dedupe evita alertar dos veces de un log que la
        // suscripción ya había traído.
        match catch_up(provider, &filter, last_seen_block, chunk_blocks).await {
            Ok((logs, tip)) => {
                last_seen_block = tip;
                for log in &logs {
                    handle_log(log, targets, db_path, tx, &mut seen).await;
                }
            }
            Err(e) => {
                tracing::warn!("recuperación de la señal 2 fallida: {e}");
                let _ = tx
                    .send(AppEvent::BackgroundError {
                        source: "vigilancia".to_string(),
                        message: format!("recuperación fallida: {e}"),
                    })
                    .await;
            }
        }

        if transport == WatchTransport::Polling {
            tokio::time::sleep(std::time::Duration::from_secs(POLL_SECS)).await;
            continue;
        }

        // Vía WebSocket: se consume hasta que el stream muera, y entonces se
        // vuelve arriba a recuperar el hueco y resuscribirse.
        match subscribe_and_consume(provider, &filter, targets, db_path, tx, &mut seen, &mut last_seen_block)
            .await
        {
            Ok(()) => {
                tracing::warn!("la suscripción de la señal 2 terminó; reconectando");
                backoff = RECONNECT_BASE_SECS;
            }
            Err(e) => {
                tracing::warn!("suscripción de la señal 2 caída: {e}; reintentando en {backoff}s");
                let _ = tx
                    .send(AppEvent::BackgroundError {
                        source: "vigilancia".to_string(),
                        message: format!("suscripción caída, reintentando en {backoff}s: {e}"),
                    })
                    .await;
                tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(RECONNECT_MAX_SECS);
            }
        }
    }
}

/// Trae los logs que hayan pasado desde `from` (exclusivo) hasta el bloque
/// actual. Devuelve también ese bloque, que pasa a ser el nuevo `last_seen`.
async fn catch_up(
    provider: &ChainProvider,
    filter: &alloy::rpc::types::Filter,
    from: u64,
    chunk_blocks: u64,
) -> anyhow::Result<(Vec<alloy::rpc::types::Log>, u64)> {
    let tip = provider.http().get_block_number().await?;
    if tip <= from {
        return Ok((Vec::new(), tip));
    }
    let logs = provider
        .get_logs_backfill(filter, from + 1, tip, chunk_blocks)
        .await?;
    Ok((logs, tip))
}

/// Se suscribe y consume hasta que el stream se acabe o falle.
async fn subscribe_and_consume(
    provider: &ChainProvider,
    filter: &alloy::rpc::types::Filter,
    targets: &Targets,
    db_path: &str,
    tx: &Sender<AppEvent>,
    seen: &mut Seen,
    last_seen_block: &mut u64,
) -> anyhow::Result<()> {
    let ws = provider.ws().await?;
    let mut sub = ws.subscribe_logs(filter).await?;

    // `recv` devuelve error cuando el canal de la suscripción se cierra: eso
    // es la caída, y se trata arriba reconectando, no aquí.
    loop {
        let log = match sub.recv().await {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!("la suscripción se cerró: {e}");
                return Ok(());
            }
        };
        if let Some(b) = log.block_number {
            *last_seen_block = (*last_seen_block).max(b);
        }
        handle_log(&log, targets, db_path, tx, seen).await;
    }
}

/// Decodifica, registra y emite una alerta.
///
/// La DB se abre por alerta en vez de mantener la conexión viva toda la
/// sesión: una alerta cada pocos minutos no justifica sostener un fichero
/// abierto, y así un fallo de disco no deja al vigilante con una conexión
/// rota que nadie repara.
async fn handle_log(
    log: &alloy::rpc::types::Log,
    targets: &Targets,
    db_path: &str,
    tx: &Sender<AppEvent>,
    seen: &mut Seen,
) {
    let (Some(tx_hash), Some(log_index)) = (log.transaction_hash, log.log_index) else {
        tracing::warn!("log de TokenLaunched sin tx_hash o log_index: no se puede deduplicar");
        let _ = tx
            .send(AppEvent::BackgroundError {
                source: "vigilancia".to_string(),
                message: "llegó un TokenLaunched sin tx_hash/log_index; ignorado".to_string(),
            })
            .await;
        return;
    };
    if !seen.insert((tx_hash, log_index)) {
        return;
    }

    let decoded = match decode_token_launched(log) {
        Ok(d) => d,
        Err(e) => {
            // No se traga: si la firma ya no coincide con la chain, hay que verlo.
            tracing::error!("TokenLaunched no decodifica: {e}");
            let _ = tx
                .send(AppEvent::BackgroundError {
                    source: "vigilancia".to_string(),
                    message: format!("TokenLaunched no decodifica: {e}"),
                })
                .await;
            return;
        }
    };

    let ev = &decoded.data;
    let deployer = ev.deployer;
    let token = ev.token;
    let label = targets.labels.get(&deployer).cloned();
    let block = log.block_number.unwrap_or(0);

    let payload = serde_json::json!({
        "token": token.to_string(),
        "curve": ev.curve.to_string(),
        "deployer": deployer.to_string(),
        "pair_token": ev.pairToken.to_string(),
        "graduation_threshold": ev.graduationThreshold.to_string(),
        "block": block,
        "tx_hash": tx_hash.to_string(),
    })
    .to_string();

    // El sink: en modo TUI el log va a `marxi.log`, nunca a stdout — escribir
    // en la pantalla mientras ratatui la posee la corrompería.
    tracing::info!(
        %deployer, %token, block,
        "ALERTA señal 2: una wallet de la watchlist ha lanzado un token"
    );

    let alert_id = match Db::open(db_path) {
        Ok(db) => match db.record_alert(&NewAlert {
            kind: AlertKind::LaunchConfirmed,
            address: deployer,
            token: Some(token),
            payload_json: payload,
            delivered: true,
        }) {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::error!("la alerta no se pudo registrar en la base: {e}");
                None
            }
        },
        Err(e) => {
            tracing::error!("no se pudo abrir la base para registrar la alerta: {e}");
            None
        }
    };

    // Se emite tenga o no `alert_id`: la alerta en pantalla no depende de que
    // el disco haya funcionado.
    let _ = tx
        .send(AppEvent::OperatorLaunchDetected {
            alert_id,
            deployer: deployer.to_string(),
            label,
            token: token.to_string(),
            pair_token: ev.pairToken.to_string(),
            graduation_threshold: ev.graduationThreshold.to_string(),
            block,
            tx_hash: tx_hash.to_string(),
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_ventana_de_vistos_no_crece_sin_limite() {
        let mut seen = Seen::new();
        for i in 0..(SEEN_CAPACITY as u64 + 100) {
            assert!(seen.insert((B256::ZERO, i)), "la clave {i} debería ser nueva");
        }
        assert_eq!(seen.set.len(), SEEN_CAPACITY);
        assert_eq!(seen.order.len(), SEEN_CAPACITY);
    }

    #[test]
    fn una_clave_repetida_no_alerta_dos_veces() {
        let mut seen = Seen::new();
        let key = (B256::repeat_byte(7), 3);
        assert!(seen.insert(key), "la primera vez es nueva");
        assert!(!seen.insert(key), "la segunda vez ya se ha visto");
    }

    #[test]
    fn una_watchlist_vacia_no_da_direcciones() {
        let list = Watchlist::default();
        let t = Targets::from_watchlist(&list).expect("una lista vacía es válida");
        assert!(t.addresses.is_empty());
    }
}
