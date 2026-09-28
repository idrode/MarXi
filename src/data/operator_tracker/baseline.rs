//! Separar **financiación de arranque** de **ingreso del negocio**, y a partir
//! de lo que quede, el `FundingBaseline`.
//!
//! Por qué hace falta: medido el 2026-09-18 sobre los dos casos extremos, la
//! inmensa mayoría de las entradas de fondos de un operador **no son
//! financiación**: son ingresos. En `0xadf3672e…`, de 2360 entradas ERC-20,
//! 1277 son el movimiento interno de su propio lanzamiento (una por
//! lanzamiento, en la misma tx que el `TokenLaunched`), 454 son ventas contra
//! su propia curva, ~594 son ventas en otro launchpad y 202 son cobros de
//! fees. **Solo 2 venían de una EOA.**
//!
//! Dos ideas que parecían razonables y que los datos descartan — no volver a
//! intentarlas:
//!
//! - **La proximidad temporal a un lanzamiento posterior no discrimina
//!   nada.** Con cadencias reales de ~2 minutos, el 76 % de los ingresos
//!   puros cae a menos de 1 minuto del siguiente lanzamiento y la ventana de
//!   24 h del diseño captura el 100 % de todo. Por eso aquí el retardo
//!   financiación→lanzamiento **se registra como dato, nunca se usa como
//!   filtro**.
//! - **El `graduationThreshold` no marca la escala del importe.**
//!   `0x4ccee0d0…` se financia con 0,0098 y 0,0499 ETH mientras su umbral es
//!   de 13,57 META: la curva nace con liquidez virtual y el umbral lo alcanzan
//!   los compradores, no el deployer. La financiación es del orden del **gas**.
//!
//! ## El criterio aprobado (usuario, 2026-09-18)
//!
//! Una entrada es financiación de arranque solo si pasa las cuatro:
//!
//! 1. **No la inicia él**: `tx.from != wallet`.
//! 2. **El remitente es una wallet** (EOA o delegada EIP-7702).
//! 3. **Una entrada nativa interna (`from == None`) nunca es financiación.**
//! 4. **El activo tiene que ser gastable por él**: ETH nativo o un token que
//!    ya ha usado como `pairToken`. Esto **no descarta, degrada** a confianza
//!    baja (probable airdrop).
//!
//! Consecuencia aceptada explícitamente por el usuario: la muestra queda
//! **diminuta** (1 financiación real en un operador de 1277 lanzamientos, 2 en
//! otro de 105). El camino normal de `judge` será `NoBaseline`, y eso es
//! correcto: relajar el criterio para tener más muestras sería volver a medir
//! ruido.
//!
//! ## Orden de las comprobaciones (por coste, no por número)
//!
//! El criterio numera "no la inicia él" primero, pero aquí se evalúa la
//! **3 → 2 → 1 → 4**. Motivo puramente de coste y el resultado es idéntico:
//! la 3 es gratis, la 2 cuesta un `eth_getCode` **por remitente distinto**
//! (que son pocos: la misma curva repetida cientos de veces) y la 1 cuesta un
//! `eth_getTransactionByHash` **por entrada** (que son miles). Filtrar antes
//! por remitente evita miles de llamadas y el 429 por unidades de cómputo de
//! Alchemy Free.

use super::history::classify_deployer;
use super::profile::{Funding, FundingAsset, OperatorProfile};
use crate::chain::ChainProvider;
use alloy::primitives::Address;
use alloy::providers::Provider;
use std::collections::{HashMap, HashSet};

/// Confianza en que una entrada clasificada como financiación lo sea.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Pasa las cuatro condiciones, activo gastable por él.
    High,
    /// Pasa el criterio pero hay motivo para desconfiar. Se cuenta, no entra
    /// en el baseline. El motivo va dentro porque no es lo mismo un airdrop
    /// que un pellizco de gas de un relay: se leen distinto.
    Low(LowReason),
}

/// Por qué una financiación queda en confianza baja.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowReason {
    /// Condición 4: el activo no es ETH nativo ni un `pairToken` que él use.
    AssetNotSpendable,
    /// Refinamiento del 2026-09-18: el remitente tiene nonce de relay.
    SenderLooksLikeInfrastructure,
}

impl Confidence {
    pub fn label(self) -> &'static str {
        match self {
            Confidence::High => "alta",
            Confidence::Low(LowReason::AssetNotSpendable) => "BAJA (activo no gastable: ¿airdrop?)",
            Confidence::Low(LowReason::SenderLooksLikeInfrastructure) => {
                "BAJA (remitente = infraestructura compartida)"
            }
        }
    }
}

/// Nonce a partir del cual un remitente se trata como **infraestructura
/// compartida**, no como la "wallet madre" de un operador.
///
/// Medido el 2026-09-18 al comprobar si las EOA financiadoras se repiten: los
/// financiadores de deployers reales se parten en dos grupos sin zona gris —
/// wallets normales con nonce 11, 111, 173, 1330 y 10 929, y un grupo con
/// nonce **325 094, 325 891, 326 827, 364 729, 686 752, 1 377 422 y
/// 2 050 320**. Las del segundo grupo mandan calldata de 3–12 KB a un mismo
/// contrato (`0xccc88a9d…`) y a Multicall3, y de paso reparten pellizcos de
/// gas a wallets sueltas: son relays/bundlers que sirven a toda la chain.
/// 100 000 deja 10× de margen sobre la wallet normal más activa observada y
/// 3× por debajo de la infraestructura menos activa.
pub const INFRA_NONCE_THRESHOLD: u64 = 100_000;

/// Límite inferior de la **zona gris** de nonce del remitente: por encima de
/// la wallet normal más activa observada (10 929) y por debajo de
/// `INFRA_NONCE_THRESHOLD`. **No interviene en la clasificación**: es una
/// regla de lectura decidida por el usuario el 2026-09-27 — un remitente en
/// `(GREY_ZONE_MIN_NONCE, INFRA_NONCE_THRESHOLD)` se marca "no confiar en la
/// clasificación automática", aunque aquí salga con confianza alta. Caso que
/// la motivó: `0x88d25c86…`, nonce 71 182, con forma de hot wallet de exchange.
pub const GREY_ZONE_MIN_NONCE: u64 = 10_929;

/// ¿Cae el nonce de un remitente en la zona gris? Ver `GREY_ZONE_MIN_NONCE`.
pub fn in_grey_zone(nonce: u64) -> bool {
    nonce > GREY_ZONE_MIN_NONCE && nonce < INFRA_NONCE_THRESHOLD
}

/// Por qué una entrada se descartó como financiación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomeReason {
    /// Nativa sin tx directa: llegó por llamada interna de un contrato.
    NativeInternal,
    /// El remitente tiene bytecode (curva, contrato de fees, distribuidor).
    SenderIsContract,
    /// La inició la propia wallet: es ella moviendo su dinero, no dinero que
    /// le llega de fuera.
    SelfInitiated,
    /// No se pudo evaluar (falló el `eth_getTransactionByHash`, o la entrada
    /// no trae `tx_hash`). **No es lo mismo que "es ingreso"**: se separa a
    /// propósito para no dar por bueno lo no comprobado.
    NotEvaluated(String),
}

/// Veredicto de una entrada concreta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FundingKind {
    Startup(Confidence),
    Income(IncomeReason),
}

/// Una entrada ya clasificada, con el contexto que hace falta para leerla.
#[derive(Debug, Clone)]
pub struct ClassifiedFunding {
    pub funding: Funding,
    pub kind: FundingKind,
    /// Nonce del remitente en el momento de mirar, si se llegó a consultar.
    /// Es lo que separa una wallet madre de un relay compartido.
    pub sender_nonce: Option<u64>,
    /// Segundos hasta el siguiente lanzamiento de la wallet, si lo hubo.
    /// **Dato, no filtro** (ver la cabecera del módulo).
    pub delay_to_next_launch: Option<u64>,
    /// La entrada es anterior al **primer** lanzamiento de la wallet. Este es
    /// el patrón fuerte observado en los dos casos: la primera entrada desde
    /// una EOA precede al primer lanzamiento (77 min en un caso, 4 h 15 en el
    /// otro). Una wallet sin lanzamientos que recibe de una EOA es la señal de
    /// más valor, más que una wallet ya conocida (que se autofinancia).
    pub precedes_first_launch: bool,
}

impl ClassifiedFunding {
    pub fn is_startup(&self) -> bool {
        matches!(self.kind, FundingKind::Startup(_))
    }
    pub fn is_high_confidence(&self) -> bool {
        matches!(self.kind, FundingKind::Startup(Confidence::High))
    }
}

/// Clasifica todas las entradas de una wallet aplicando el criterio aprobado.
///
/// `fundings` se consume: cada entrada sale envuelta en su clasificación, para
/// que no queden dos listas que puedan desincronizarse.
pub async fn classify_fundings(
    provider: &ChainProvider,
    profile: &OperatorProfile,
    fundings: Vec<Funding>,
) -> anyhow::Result<Vec<ClassifiedFunding>> {
    let wallet = profile.address;
    let spendable = spendable_assets(profile);
    let first_launch_ts = profile.launches.first().map(|l| l.timestamp);

    // Un `eth_getCode` por remitente distinto, no por entrada.
    let mut sender_is_wallet: HashMap<Address, bool> = HashMap::new();
    let mut sender_nonce: HashMap<Address, u64> = HashMap::new();

    let mut out = Vec::with_capacity(fundings.len());
    for funding in fundings {
        let kind = classify_one(
            provider,
            wallet,
            &spendable,
            &mut sender_is_wallet,
            &mut sender_nonce,
            &funding,
        )
        .await;
        let nonce = funding.from.and_then(|a| sender_nonce.get(&a).copied());

        let delay_to_next_launch = profile
            .launches
            .iter()
            .find(|l| l.timestamp >= funding.timestamp)
            .map(|l| l.timestamp - funding.timestamp);
        let precedes_first_launch = match first_launch_ts {
            Some(ts) => funding.timestamp > 0 && funding.timestamp < ts,
            None => true, // sin lanzamientos todavía: toda entrada es previa
        };

        out.push(ClassifiedFunding {
            funding,
            kind,
            sender_nonce: nonce,
            delay_to_next_launch,
            precedes_first_launch,
        });
    }
    Ok(out)
}

async fn classify_one(
    provider: &ChainProvider,
    wallet: Address,
    spendable: &HashSet<Address>,
    sender_is_wallet: &mut HashMap<Address, bool>,
    sender_nonce: &mut HashMap<Address, u64>,
    funding: &Funding,
) -> FundingKind {
    // (3) nativa interna: nunca es financiación. Gratis, va primero.
    let Some(from) = funding.from else {
        return FundingKind::Income(IncomeReason::NativeInternal);
    };

    // (2) el remitente tiene que ser una wallet. Cacheado por remitente.
    let is_wallet = match sender_is_wallet.get(&from) {
        Some(v) => *v,
        None => {
            let v = classify_deployer(provider, from).await.is_wallet();
            sender_is_wallet.insert(from, v);
            v
        }
    };
    if !is_wallet {
        return FundingKind::Income(IncomeReason::SenderIsContract);
    }

    // (1) no la inicia él. Una llamada por entrada, pero ya quedan pocas.
    let Some(tx_hash) = funding.tx_hash else {
        return FundingKind::Income(IncomeReason::NotEvaluated(
            "la entrada no trae tx_hash".into(),
        ));
    };
    match tx_initiator(provider, &tx_hash.to_string()).await {
        Ok(Some(initiator)) if initiator == wallet => {
            FundingKind::Income(IncomeReason::SelfInitiated)
        }
        Ok(Some(_)) => {
            // (4) activo gastable: degrada, no descarta.
            let gastable = match &funding.asset {
                FundingAsset::Native => true,
                FundingAsset::Erc20 { token, .. } => spendable.contains(token),
            };
            // Refinamiento medido el 2026-09-18, POSTERIOR al criterio
            // aprobado: un remitente con nonce de cientos de miles es un relay
            // compartido, no una wallet madre. Degrada igual que el activo no
            // gastable —no descarta— porque el criterio aprobado son cuatro
            // condiciones y esta es una quinta señal, no un veto.
            let nonce = match sender_nonce.get(&from) {
                Some(n) => Some(*n),
                None => match provider.http().get_transaction_count(from).await {
                    Ok(n) => {
                        sender_nonce.insert(from, n);
                        Some(n)
                    }
                    Err(e) => {
                        tracing::warn!(%from, "no se pudo leer el nonce del remitente: {e}");
                        None
                    }
                },
            };
            let parece_infra = nonce.is_some_and(|n| n >= INFRA_NONCE_THRESHOLD);
            FundingKind::Startup(if !gastable {
                Confidence::Low(LowReason::AssetNotSpendable)
            } else if parece_infra {
                Confidence::Low(LowReason::SenderLooksLikeInfrastructure)
            } else {
                Confidence::High
            })
        }
        Ok(None) => FundingKind::Income(IncomeReason::NotEvaluated(
            "la tx no trae campo `from`".into(),
        )),
        Err(e) => FundingKind::Income(IncomeReason::NotEvaluated(e.to_string())),
    }
}

/// Quién firmó la tx. Se pide como JSON crudo por la misma razón que el bloque
/// en `funding::record_native`: esta chain (Arbitrum Nitro) tiene tipos de tx
/// propios que el tipo `Transaction` de alloy rechaza entero.
async fn tx_initiator(provider: &ChainProvider, tx_hash: &str) -> anyhow::Result<Option<Address>> {
    let raw: serde_json::Value = provider
        .http()
        .raw_request("eth_getTransactionByHash".into(), (tx_hash.to_string(),))
        .await?;
    Ok(raw
        .get("from")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<Address>().ok()))
}

/// Activos que la wallet sabe gastar: ETH nativo (implícito) y los tokens que
/// ya ha usado como `pairToken` en algún lanzamiento.
fn spendable_assets(profile: &OperatorProfile) -> HashSet<Address> {
    profile
        .launches
        .iter()
        .map(|l| l.pair_token)
        .filter(|a| !a.is_zero())
        .collect()
}

/// Baseline de financiación de un activo concreto: con qué importes se
/// financia esta wallet y desde dónde.
///
/// Se construye **solo** con entradas `Startup(High)`. Con el criterio
/// aprobado eso significa, en los casos medidos, una o dos muestras: los
/// campos están igualmente porque el tipo describe el conjunto, sea del
/// tamaño que sea, y `samples` dice lo pequeño que es.
#[derive(Debug, Clone)]
pub struct FundingBaseline {
    pub asset: FundingAsset,
    pub samples: usize,
    pub min: f64,
    pub median: f64,
    pub max: f64,
    /// Mediana del retardo hasta el siguiente lanzamiento. **Descriptivo**:
    /// medido que el retardo no discrimina financiación de ingreso.
    pub median_delay_secs: u64,
    /// Remitentes que aparecen en más de una financiación de esta wallet.
    /// Son los candidatos a "wallet madre" y, según lo medido, lo que de
    /// verdad conviene vigilar: un operador ya activo se autofinancia con sus
    /// ingresos, así que da poca anticipación; su financiador, en cambio,
    /// puede estar arrancando al siguiente operador.
    pub recurring_sources: Vec<Address>,
}

/// Cuántas muestras hacen falta para que la mediana signifique algo. Por
/// debajo de esto el baseline existe pero `judge` no compara: devuelve
/// `NoBaseline` en vez de fingir una referencia construida sobre un dato.
pub const MIN_BASELINE_SAMPLES: usize = 3;

/// Construye un baseline por cada activo con financiación de alta confianza.
pub fn build_baselines(classified: &[ClassifiedFunding]) -> Vec<FundingBaseline> {
    let mut by_asset: HashMap<String, (FundingAsset, Vec<&ClassifiedFunding>)> = HashMap::new();
    for c in classified.iter().filter(|c| c.is_high_confidence()) {
        let key = match &c.funding.asset {
            FundingAsset::Native => "native".to_string(),
            FundingAsset::Erc20 { token, .. } => token.to_string(),
        };
        by_asset
            .entry(key)
            .or_insert_with(|| (c.funding.asset.clone(), Vec::new()))
            .1
            .push(c);
    }

    let mut out: Vec<FundingBaseline> = by_asset
        .into_values()
        .map(|(asset, items)| {
            let mut amounts: Vec<f64> = items.iter().map(|c| c.funding.amount).collect();
            amounts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mut delays: Vec<u64> = items
                .iter()
                .filter_map(|c| c.delay_to_next_launch)
                .collect();
            delays.sort_unstable();

            let mut seen: HashMap<Address, usize> = HashMap::new();
            for c in &items {
                if let Some(from) = c.funding.from {
                    *seen.entry(from).or_default() += 1;
                }
            }
            let mut recurring: Vec<Address> =
                seen.into_iter().filter(|(_, n)| *n > 1).map(|(a, _)| a).collect();
            recurring.sort_unstable();

            FundingBaseline {
                asset,
                samples: amounts.len(),
                min: *amounts.first().unwrap_or(&0.0),
                median: amounts[amounts.len() / 2],
                max: *amounts.last().unwrap_or(&0.0),
                median_delay_secs: delays.get(delays.len() / 2).copied().unwrap_or(0),
                recurring_sources: recurring,
            }
        })
        .collect();
    out.sort_by(|a, b| b.samples.cmp(&a.samples));
    out
}

/// Veredicto de una financiación nueva contra el baseline.
#[derive(Debug, Clone, PartialEq)]
pub enum FundingVerdict {
    /// Dentro de `[0.5×, 2×]` de la mediana.
    MatchesPattern { ratio: f64 },
    Unusual { ratio: f64 },
    /// No hay con qué comparar (menos de `MIN_BASELINE_SAMPLES` muestras para
    /// ese activo). **Es el camino normal, no un fallo**, y no silencia la
    /// alerta: se emite diciendo que no hay baseline.
    NoBaseline,
    /// Por debajo del suelo de ruido: ni se alerta.
    BelowNoiseFloor,
    /// La entrada no pasa el criterio: es ingreso del negocio, no financiación.
    NotFunding(IncomeReason),
}

/// Suelo de ruido para ETH nativo, en ETH.
///
/// Calibrado con lo medido: la financiación real más pequeña observada es de
/// **0,0098 ETH** (`0x4ccee0d0…`, 4 h 15 antes de su primer lanzamiento), así
/// que el suelo tiene que quedar claramente por debajo. 0,0001 ETH deja dos
/// órdenes de magnitud de margen y sigue filtrando el polvo.
pub const DEFAULT_NATIVE_NOISE_FLOOR: f64 = 0.000_1;

/// Juzga una entrada ya clasificada contra los baselines de la wallet.
///
/// Para ERC-20 no hay suelo de ruido: el criterio (activo gastable) ya degrada
/// el polvo y los airdrops a confianza baja, y poner un umbral en unidades de
/// un token cualquiera no significaría nada.
pub fn judge(
    baselines: &[FundingBaseline],
    classified: &ClassifiedFunding,
    native_noise_floor: f64,
) -> FundingVerdict {
    match &classified.kind {
        FundingKind::Income(reason) => return FundingVerdict::NotFunding(reason.clone()),
        FundingKind::Startup(_) => {}
    }
    if classified.funding.asset.is_native() && classified.funding.amount < native_noise_floor {
        return FundingVerdict::BelowNoiseFloor;
    }
    let base = baselines
        .iter()
        .find(|b| same_asset(&b.asset, &classified.funding.asset))
        .filter(|b| b.samples >= MIN_BASELINE_SAMPLES);
    let Some(base) = base else {
        return FundingVerdict::NoBaseline;
    };
    if base.median <= 0.0 {
        return FundingVerdict::NoBaseline;
    }
    let ratio = classified.funding.amount / base.median;
    if (0.5..=2.0).contains(&ratio) {
        FundingVerdict::MatchesPattern { ratio }
    } else {
        FundingVerdict::Unusual { ratio }
    }
}

fn same_asset(a: &FundingAsset, b: &FundingAsset) -> bool {
    match (a, b) {
        (FundingAsset::Native, FundingAsset::Native) => true,
        (FundingAsset::Erc20 { token: x, .. }, FundingAsset::Erc20 { token: y, .. }) => x == y,
        _ => false,
    }
}

/// Recuento por veredicto, para poder decir en una línea qué proporción de las
/// entradas era ingreso — que es el resultado principal de la medición.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClassificationSummary {
    pub total: usize,
    pub startup_high: usize,
    pub startup_low: usize,
    pub native_internal: usize,
    pub sender_is_contract: usize,
    pub self_initiated: usize,
    pub not_evaluated: usize,
}

pub fn summarize(classified: &[ClassifiedFunding]) -> ClassificationSummary {
    let mut s = ClassificationSummary { total: classified.len(), ..Default::default() };
    for c in classified {
        match &c.kind {
            FundingKind::Startup(Confidence::High) => s.startup_high += 1,
            FundingKind::Startup(Confidence::Low(_)) => s.startup_low += 1,
            FundingKind::Income(IncomeReason::NativeInternal) => s.native_internal += 1,
            FundingKind::Income(IncomeReason::SenderIsContract) => s.sender_is_contract += 1,
            FundingKind::Income(IncomeReason::SelfInitiated) => s.self_initiated += 1,
            FundingKind::Income(IncomeReason::NotEvaluated(_)) => s.not_evaluated += 1,
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    fn f(amount: f64, from: Option<Address>) -> Funding {
        Funding {
            asset: FundingAsset::Native,
            from,
            amount_raw: U256::ZERO,
            amount,
            block: 1,
            timestamp: 100,
            tx_hash: None,
        }
    }
    fn c(amount: f64, kind: FundingKind, from: Option<Address>) -> ClassifiedFunding {
        ClassifiedFunding {
            funding: f(amount, from),
            kind,
            sender_nonce: None,
            delay_to_next_launch: Some(60),
            precedes_first_launch: false,
        }
    }

    #[test]
    fn baseline_solo_con_alta_confianza() {
        let items = vec![
            c(1.0, FundingKind::Startup(Confidence::High), None),
            c(9.0, FundingKind::Startup(Confidence::Low(LowReason::AssetNotSpendable)), None),
            c(9.0, FundingKind::Income(IncomeReason::SelfInitiated), None),
        ];
        let b = build_baselines(&items);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].samples, 1);
        assert_eq!(b[0].median, 1.0);
    }

    #[test]
    fn muestra_diminuta_da_nobaseline() {
        // El caso normal con el criterio aprobado: una sola muestra.
        let items = vec![c(1.0, FundingKind::Startup(Confidence::High), None)];
        let b = build_baselines(&items);
        let nueva = c(1.0, FundingKind::Startup(Confidence::High), None);
        assert_eq!(judge(&b, &nueva, DEFAULT_NATIVE_NOISE_FLOOR), FundingVerdict::NoBaseline);
    }

    #[test]
    fn con_muestras_suficientes_compara() {
        let items: Vec<_> = [1.0, 1.0, 1.0]
            .iter()
            .map(|a| c(*a, FundingKind::Startup(Confidence::High), None))
            .collect();
        let b = build_baselines(&items);
        assert_eq!(b[0].samples, 3);
        match judge(&b, &c(1.5, FundingKind::Startup(Confidence::High), None), DEFAULT_NATIVE_NOISE_FLOOR) {
            FundingVerdict::MatchesPattern { ratio } => assert!((ratio - 1.5).abs() < 1e-9),
            v => panic!("esperaba MatchesPattern, salió {v:?}"),
        }
        match judge(&b, &c(5.0, FundingKind::Startup(Confidence::High), None), DEFAULT_NATIVE_NOISE_FLOOR) {
            FundingVerdict::Unusual { .. } => {}
            v => panic!("esperaba Unusual, salió {v:?}"),
        }
    }

    #[test]
    fn el_suelo_de_ruido_no_se_come_la_financiacion_real_medida() {
        // 0,0098 ETH es una financiación real observada: no puede filtrarse.
        let items: Vec<_> = [1.0, 1.0, 1.0]
            .iter()
            .map(|a| c(*a, FundingKind::Startup(Confidence::High), None))
            .collect();
        let b = build_baselines(&items);
        assert_ne!(
            judge(&b, &c(0.009_848, FundingKind::Startup(Confidence::High), None), DEFAULT_NATIVE_NOISE_FLOOR),
            FundingVerdict::BelowNoiseFloor
        );
        assert_eq!(
            judge(&b, &c(0.000_01, FundingKind::Startup(Confidence::High), None), DEFAULT_NATIVE_NOISE_FLOOR),
            FundingVerdict::BelowNoiseFloor
        );
    }

    #[test]
    fn ingreso_nunca_es_financiacion() {
        let b = vec![];
        let v = judge(&b, &c(1.0, FundingKind::Income(IncomeReason::NativeInternal), None), DEFAULT_NATIVE_NOISE_FLOOR);
        assert_eq!(v, FundingVerdict::NotFunding(IncomeReason::NativeInternal));
    }
}
