//! Construcción del provider alloy para Robinhood Chain.
//!
//! Nota de arquitectura: se mantienen dos transports posibles —
//! HTTP para llamadas puntuales (eth_call, eth_getLogs de backfill,
//! envío de tx) y WebSocket para suscripciones en tiempo real
//! (eth_subscribe a logs de factories/launchpads). No usar solo HTTP con
//! polling para el sniper: la latencia de detección es la ventaja competitiva
//! de todo este proyecto.
//!
//! Invariante: `connect` comprueba `eth_chainId` contra `cfg.chain_id` y
//! falla si no coinciden — no es posible operar contra una chain distinta
//! de la configurada, sea cual sea el rpc_provider.
//!
//! **Provider dual (Paso 1, 2026-09-14).** No hay un solo RPC que sirva para
//! todo en esta chain con las cuentas disponibles:
//!
//! - Alchemy (tier Free) responde bien a `eth_call`, recibos y WebSocket,
//!   pero **limita `eth_getLogs` a 10 bloques por petición**, lo que lo hace
//!   inútil para traer el histórico de un token.
//! - El RPC público oficial sí sirve `eth_getLogs` de rango amplio filtrando
//!   por `address` + `topics`, pero devuelve `429` ante ráfagas de `eth_call`
//!   y `-32000 log query timed out` si el rango es de decenas de millones de
//!   bloques.
//!
//! Por eso `ChainProvider` mantiene dos transports HTTP: `http()` para calls
//! y `logs()` para backfill, más `get_logs_backfill` que trocea el rango y
//! reintenta. Medido y documentado en CLAUDE.md ("Límites de RPC
//! descubiertos"). Cuando `rpc_provider = "public"` ambos son el mismo
//! provider y no se duplica conexión.

use crate::config::ChainConfig;
use alloy::providers::{DynProvider, Provider, ProviderBuilder, WsConnect};
use alloy::rpc::types::{Filter, Log};

/// Cuántas veces se reintenta un `eth_getLogs` que falla (429 del público,
/// timeout de consulta) antes de partir el tramo en dos.
const LOG_RETRIES: u32 = 3;
/// Por debajo de este ancho ya no se parte el tramo: si un rango tan pequeño
/// sigue fallando, no es que sea grande, es que el RPC no está sirviendo, y
/// entonces hay que fallar en vez de subdividir hasta el bloque suelto.
const MIN_CHUNK_BLOCKS: u64 = 64;
/// Espera base entre reintentos; se multiplica por el número de intento.
const RETRY_BASE_SECS: u64 = 3;

pub struct ChainProvider {
    pub chain_id: u64,
    /// Guardada solo para diagnóstico futuro; nunca se loguea.
    #[allow(dead_code)]
    http_url: String,
    ws_url: Option<String>,
    http: DynProvider,
    /// Transport para `eth_getLogs` de rango amplio. Puede ser el mismo que
    /// `http` si el rpc_provider configurado ya es el público.
    logs: DynProvider,
}

impl ChainProvider {
    /// Resuelve qué RPC usar según `rpc_provider` en config + variables de
    /// entorno con las API keys. Nunca loguear la URL completa si contiene
    /// una API key en el path (Alchemy la pone en la propia URL).
    pub fn resolve_rpc_urls(cfg: &ChainConfig) -> anyhow::Result<(String, Option<String>)> {
        match cfg.rpc_provider.as_str() {
            "alchemy" => {
                let key = std::env::var("ALCHEMY_API_KEY").map_err(|_| {
                    anyhow::anyhow!("rpc_provider = \"alchemy\" pero falta ALCHEMY_API_KEY en el entorno")
                })?;
                // El subdominio depende de la chain. Solo mainnet está
                // confirmado (docs de Alchemy); para testnet no se ha
                // verificado ningún subdominio, así que se falla en vez de
                // conectar silenciosamente a mainnet.
                let host = match cfg.chain_id {
                    4663 => "robinhood-mainnet.g.alchemy.com",
                    46630 => anyhow::bail!(
                        "rpc_provider = \"alchemy\" con chain_id = 46630 (testnet): el subdominio \
                         de Alchemy para testnet no está verificado. Usar rpc_provider = \"public\" \
                         con rpc_http_fallback apuntando a la testnet"
                    ),
                    other => anyhow::bail!("chain_id = {other} no soportado con rpc_provider = \"alchemy\""),
                };
                let http = format!("https://{host}/v2/{key}");
                let ws = format!("wss://{host}/v2/{key}");
                Ok((http, Some(ws)))
            }
            "quicknode" => {
                let url = std::env::var("QUICKNODE_URL")
                    .map_err(|_| anyhow::anyhow!("falta QUICKNODE_URL en el entorno"))?;
                Ok((url.clone(), Some(url)))
            }
            "chainstack" => {
                let url = std::env::var("CHAINSTACK_URL")
                    .map_err(|_| anyhow::anyhow!("falta CHAINSTACK_URL en el entorno"))?;
                Ok((url.clone(), Some(url)))
            }
            "public" => {
                tracing::warn!(
                    "usando RPC público de Robinhood Chain: rate-limited, no apto para \
                     sniping en producción según la documentación oficial"
                );
                Ok((cfg.rpc_http_fallback.clone(), None))
            }
            other => anyhow::bail!("rpc_provider desconocido en config: {other}"),
        }
    }

    /// Construye el provider HTTP y confirma con `eth_chainId` que el RPC
    /// realmente sirve la chain configurada. Los mensajes de error no
    /// incluyen la URL para no filtrar la API key.
    pub async fn connect(cfg: &ChainConfig) -> anyhow::Result<Self> {
        let (http_url, ws_url) = Self::resolve_rpc_urls(cfg)?;
        let url: reqwest::Url = http_url
            .parse()
            .map_err(|e| anyhow::anyhow!("URL HTTP del rpc_provider {:?} inválida: {e}", cfg.rpc_provider))?;
        let http = ProviderBuilder::new().connect_http(url).erased();

        let remote = http.get_chain_id().await.map_err(|e| {
            anyhow::anyhow!("eth_chainId falló contra rpc_provider {:?}: {e}", cfg.rpc_provider)
        })?;
        if remote != cfg.chain_id {
            anyhow::bail!(
                "el RPC ({}) sirve chain_id {remote} pero config dice {} — abortando",
                cfg.rpc_provider,
                cfg.chain_id
            );
        }
        tracing::info!(rpc_provider = %cfg.rpc_provider, chain_id = remote, "provider HTTP conectado");

        // Segundo transport, solo para eth_getLogs de rango amplio. Si el
        // provider configurado YA es el público, no se abre otro.
        let logs = if cfg.rpc_provider == "public" {
            http.clone()
        } else {
            let logs_url: reqwest::Url = cfg.rpc_http_fallback.parse().map_err(|e| {
                anyhow::anyhow!("config chain.rpc_http_fallback no es una URL válida: {e}")
            })?;
            let logs = ProviderBuilder::new().connect_http(logs_url).erased();
            // Mismo invariante que el transport principal: si el RPC de logs
            // sirviera otra chain, el backfill mezclaría historias distintas.
            let logs_chain = logs.get_chain_id().await.map_err(|e| {
                anyhow::anyhow!("eth_chainId falló contra el RPC público de logs: {e}")
            })?;
            if logs_chain != cfg.chain_id {
                anyhow::bail!(
                    "el RPC de logs (chain.rpc_http_fallback) sirve chain_id {logs_chain} \
                     pero config dice {} — abortando",
                    cfg.chain_id
                );
            }
            tracing::info!(chain_id = logs_chain, "provider de logs (RPC público) conectado");
            logs
        };

        Ok(Self {
            chain_id: cfg.chain_id,
            http_url,
            ws_url,
            http,
            logs,
        })
    }

    /// Provider HTTP compartido (llamadas puntuales, envío de tx).
    /// **No usarlo para `eth_getLogs` de rango amplio:** ver `logs()`.
    pub fn http(&self) -> &DynProvider {
        &self.http
    }

    /// Provider para `eth_getLogs` de rango amplio. Con `rpc_provider =
    /// "alchemy"` es el RPC público, porque Alchemy Free corta en 10 bloques.
    #[allow(dead_code)] // API del provider dual; `get_logs_backfill` usa el campo directamente
    pub fn logs(&self) -> &DynProvider {
        &self.logs
    }

    /// `eth_getLogs` sobre un rango arbitrariamente grande, troceado en
    /// tramos de `chunk_blocks`, con reintentos y **subdivisión del tramo que
    /// no pasa**.
    ///
    /// Necesario porque el RPC público agota el tiempo (`-32000 log query
    /// timed out`) en rangos grandes y devuelve `429` bajo carga.
    ///
    /// Los dos fallos no son iguales y por eso no basta con reintentar:
    ///
    /// - el `429` es carga momentánea y se pasa esperando;
    /// - el **timeout depende del trabajo que el tramo le cuesta al nodo**, y
    ///   ese trabajo no baja por esperar. Un tramo que agota el tiempo lo
    ///   vuelve a agotar las tres veces, y antes eso abortaba el backfill
    ///   entero dejando al buscador sin datos.
    ///
    /// Por eso, agotados los reintentos, el tramo se **parte en dos y se
    /// reintenta cada mitad** (y así recursivamente): cada partición reduce a
    /// la mitad el trabajo por petición, que es la variable que de verdad
    /// manda. Solo se abandona cuando un tramo ya menor de
    /// `MIN_CHUNK_BLOCKS` sigue fallando — ahí el problema no es el tamaño.
    ///
    /// Los logs se devuelven en orden de bloque ascendente, también cuando ha
    /// habido subdivisión.
    ///
    /// `base` debe traer ya `address` y `topics`; su rango de bloques se
    /// ignora y lo fija esta función.
    pub async fn get_logs_backfill(
        &self,
        base: &Filter,
        from_block: u64,
        to_block: u64,
        chunk_blocks: u64,
    ) -> anyhow::Result<Vec<Log>> {
        anyhow::ensure!(chunk_blocks > 0, "chunk_blocks debe ser > 0");
        if from_block > to_block {
            return Ok(Vec::new());
        }

        // Pila de tramos pendientes, en orden ascendente. Un tramo que falla
        // se sustituye por sus dos mitades **en su sitio**, así que la salida
        // sigue en orden de bloque aunque haya habido subdivisión.
        let mut pending: std::collections::VecDeque<(u64, u64)> = std::collections::VecDeque::new();
        let mut from = from_block;
        while from <= to_block {
            let to = from.saturating_add(chunk_blocks - 1).min(to_block);
            pending.push_back((from, to));
            from = to + 1;
        }

        let mut out = Vec::new();
        while let Some((from, to)) = pending.pop_front() {
            let filter = base.clone().from_block(from).to_block(to);

            let mut attempt = 0;
            let part = loop {
                match self.logs.get_logs(&filter).await {
                    Ok(v) => break Some(v),
                    Err(e) if attempt < LOG_RETRIES => {
                        attempt += 1;
                        let wait = RETRY_BASE_SECS * attempt as u64;
                        tracing::warn!(
                            from, to, attempt, wait_secs = wait,
                            "eth_getLogs falló ({e}); reintentando"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    }
                    Err(e) => match split_range(from, to) {
                        // El tramo es grande: partirlo baja el trabajo por
                        // petición, que es lo que provoca el timeout.
                        Some((lo, hi)) => {
                            tracing::warn!(
                                from, to, span = to - from + 1,
                                "eth_getLogs sigue fallando ({e}); partiendo el tramo en dos"
                            );
                            pending.push_front(hi);
                            pending.push_front(lo);
                            break None;
                        }
                        // Ya es pequeño y sigue fallando: no es el tamaño.
                        None => {
                            return Err(anyhow::anyhow!(
                                "eth_getLogs falló en el tramo {from}-{to} ({} bloques) tras \
                                 {LOG_RETRIES} reintentos y sin margen para partirlo más: {e}",
                                to - from + 1
                            ))
                        }
                    },
                }
            };
            if let Some(part) = part {
                tracing::debug!(from, to, logs = part.len(), "tramo de backfill resuelto");
                out.extend(part);
            }
        }
        Ok(out)
    }

    /// Abre una conexión WebSocket nueva para suscripciones. Cada llamada
    /// crea su propio socket; comprueba también `eth_chainId`.
    pub async fn ws(&self) -> anyhow::Result<DynProvider> {
        let url = self.ws_url.as_deref().ok_or_else(|| {
            anyhow::anyhow!("el rpc_provider configurado no ofrece WebSocket (¿rpc_provider = \"public\"?)")
        })?;
        let ws = ProviderBuilder::new()
            .connect_ws(WsConnect::new(url))
            .await
            .map_err(|e| anyhow::anyhow!("conexión WebSocket fallida: {e}"))?
            .erased();
        let remote = ws.get_chain_id().await?;
        if remote != self.chain_id {
            anyhow::bail!("el WS sirve chain_id {remote} pero se esperaba {}", self.chain_id);
        }
        Ok(ws)
    }

    /// Solo para diagnósticos que necesiten saber si hay WS disponible; no
    /// expone la URL.
    pub fn has_ws(&self) -> bool {
        self.ws_url.is_some()
    }
}

/// Parte `[from, to]` en dos mitades contiguas. `None` si el tramo ya es
/// menor que `MIN_CHUNK_BLOCKS`: por debajo de ahí partir más no arregla un
/// timeout, solo multiplica las peticiones contra un RPC que ya está
/// fallando.
fn split_range(from: u64, to: u64) -> Option<((u64, u64), (u64, u64))> {
    let span = to.saturating_sub(from).saturating_add(1);
    if span < MIN_CHUNK_BLOCKS * 2 {
        return None;
    }
    let mid = from + span / 2 - 1;
    Some(((from, mid), (mid + 1, to)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partir_cubre_el_rango_entero_sin_solaparse() {
        let (lo, hi) = split_range(1000, 5999).unwrap();
        assert_eq!(lo.0, 1000);
        assert_eq!(hi.1, 5999);
        assert_eq!(lo.1 + 1, hi.0, "las mitades deben ser contiguas y no solaparse");
        assert_eq!((lo.1 - lo.0 + 1) + (hi.1 - hi.0 + 1), 5000);
    }

    #[test]
    fn partir_un_tramo_pequeno_devuelve_none() {
        // Por debajo del doble del mínimo ya no se parte: el problema no es
        // el tamaño y subdividir solo castigaría más al RPC.
        assert!(split_range(0, MIN_CHUNK_BLOCKS * 2 - 2).is_none());
        assert!(split_range(42, 42).is_none());
    }

    #[test]
    fn partir_repetido_converge_al_minimo() {
        // Un tramo de 5M debe poder partirse hasta el mínimo en pocos pasos,
        // sin bucle infinito ni tramos de ancho 0.
        let (mut from, mut to) = (0u64, 4_999_999u64);
        let mut steps = 0;
        while let Some((lo, _hi)) = split_range(from, to) {
            assert!(lo.1 >= lo.0);
            from = lo.0;
            to = lo.1;
            steps += 1;
            assert!(steps < 64, "la subdivisión no converge");
        }
        assert!(to - from + 1 < MIN_CHUNK_BLOCKS * 2);
    }
}
