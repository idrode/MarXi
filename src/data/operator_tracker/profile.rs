//! Tipos del perfil de un operador de launchpad.
//!
//! Estado: pobladas las dos partes que ya se miden contra la chain, los
//! **lanzamientos** (`PastLaunch`) y las **financiaciones** (`Funding`,
//! ERC-20 por logs y ETH nativo por diferencias de saldo). `FundingBaseline`
//! (la comparación "este depósito se parece a los que preceden a un
//! lanzamiento") todavía NO está: necesita ver el reparto real primero, y no
//! se declara un tipo vacío que aparente trabajo hecho.

use alloy::primitives::{Address, B256, U256};

/// Un lanzamiento pasado de un deployer, tal y como sale de `TokenLaunched`.
#[derive(Debug, Clone)]
pub struct PastLaunch {
    pub token: Address,
    pub curve: Address,
    pub pair_token: Address,
    pub graduation_threshold: alloy::primitives::U256,
    pub block: u64,
    /// Timestamp del bloque. Puede venir interpolado (ver
    /// `backfill::resolve_block_timestamps`): sirve para ordenar y medir
    /// cadencia, no como dato exacto de auditoría.
    pub timestamp: u64,
    pub tx_hash: B256,
}

/// En qué activo llegó una financiación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FundingAsset {
    /// ETH nativo. No emite `Transfer`: se detecta por diferencias de saldo
    /// (ver `funding::find_native_fundings`).
    Native,
    Erc20 { token: Address, decimals: u8 },
}

impl FundingAsset {
    pub fn is_native(&self) -> bool {
        matches!(self, FundingAsset::Native)
    }
}

/// Una entrada de fondos a la wallet del operador.
#[derive(Debug, Clone)]
pub struct Funding {
    pub asset: FundingAsset,
    /// Quién envió. `None` en una entrada nativa que no viene de una tx
    /// directa (la mandó un contrato por llamada interna): el bloque se
    /// conoce, el remitente no, y eso se dice en vez de inventarlo.
    pub from: Option<Address>,
    pub amount_raw: U256,
    pub amount: f64,
    pub block: u64,
    pub timestamp: u64,
    pub tx_hash: Option<B256>,
}

/// Qué es la dirección que figura como `deployer` en `TokenLaunched`.
///
/// No es un detalle cosmético: el "deployer" más prolífico de la chain es
/// **Multicall3** (`0xca11bde0…ca11`, 4.791 lanzamientos, 58 pairTokens
/// distintos, medido el 2026-09-17), es decir un contrato relay por el que
/// pasan lanzamientos de terceros. Toda la premisa del tracker —"se financia
/// a la wallet antes de lanzar"— no aplica a una dirección así: no se
/// financia para lanzar, y sus lanzamientos no son de un solo operador.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployerKind {
    /// Sin bytecode: una EOA, que es lo que el tracker espera.
    Wallet,
    /// EOA con delegación **EIP-7702**: `eth_getCode` devuelve 23 bytes
    /// `0xef0100 ‖ address`. Tiene bytecode pero **es una wallet**, con su
    /// clave privada y su saldo, así que la premisa del tracker sí le
    /// aplica. Caso real medido el 2026-09-17:
    /// `0xadf3672e…` delegaba en `0x63c0c19a…`.
    DelegatedWallet { delegate: Address },
    /// Con bytecode. Se marca, **no se bloquea**: puede interesar entender un
    /// relay concreto. Pero no se trata como un operador ni se vigila su
    /// financiación sin decidirlo a mano.
    PossibleRelayContract,
    /// No se pudo comprobar (el `eth_getCode` falló). Distinto de "es una
    /// wallet": no confundir no evaluado con evaluado y limpio.
    Unknown,
}

impl DeployerKind {
    pub fn label(self) -> String {
        match self {
            DeployerKind::Wallet => "wallet (EOA)".to_string(),
            DeployerKind::DelegatedWallet { delegate } => {
                format!("wallet con delegación EIP-7702 → {delegate}")
            }
            DeployerKind::PossibleRelayContract => "POSIBLE CONTRATO RELAY, NO WALLET".to_string(),
            DeployerKind::Unknown => "sin comprobar".to_string(),
        }
    }

    /// Si esto es true, la dirección no debería entrar en la watchlist ni
    /// generar alertas de financiación sin una decisión explícita.
    ///
    /// Una wallet delegada por 7702 **no** avisa: sigue siendo una wallet.
    pub fn needs_warning(self) -> bool {
        matches!(self, DeployerKind::PossibleRelayContract | DeployerKind::Unknown)
    }

    /// ¿Se puede tratar como operador (financiable, vigilable)?
    pub fn is_wallet(self) -> bool {
        matches!(self, DeployerKind::Wallet | DeployerKind::DelegatedWallet { .. })
    }
}

/// Perfil de un operador: por ahora, su historial de lanzamientos y el rango
/// de bloques en el que se buscó (para saber qué cubre y qué no).
#[derive(Debug, Clone)]
pub struct OperatorProfile {
    pub address: Address,
    pub label: Option<String>,
    /// Resultado de `chain::verify::has_bytecode` sobre `address`.
    pub kind: DeployerKind,
    pub launches: Vec<PastLaunch>,
    pub history_from_block: u64,
    pub history_to_block: u64,
}

impl OperatorProfile {
    /// Segundos entre lanzamientos consecutivos. Vacío con menos de dos.
    pub fn intervals_secs(&self) -> Vec<u64> {
        self.launches
            .windows(2)
            .map(|w| w[1].timestamp.saturating_sub(w[0].timestamp))
            .collect()
    }

    /// Mediana de los intervalos, como primera medida de cadencia del
    /// operador. No es una predicción: solo describe lo ya observado.
    pub fn median_interval_secs(&self) -> Option<u64> {
        let mut v = self.intervals_secs();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        Some(v[v.len() / 2])
    }

    /// Cuántos pares distintos ha usado. Un operador que siempre lanza contra
    /// el mismo pairToken es una señal de rutina; variarlo, de lo contrario.
    pub fn distinct_pair_tokens(&self) -> Vec<(Address, usize)> {
        let mut counts: std::collections::HashMap<Address, usize> = std::collections::HashMap::new();
        for l in &self.launches {
            *counts.entry(l.pair_token).or_default() += 1;
        }
        let mut v: Vec<(Address, usize)> = counts.into_iter().collect();
        v.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        v
    }
}
