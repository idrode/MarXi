//! Motor de swap contra Uniswap V4 para tokens ya graduados. A diferencia de
//! V2/V3, V4 usa un singleton (`PoolManager`) compartido por toda la chain
//! en vez de un contrato de par por cada pool — hay que operar a través de
//! ese singleton (o de un router que lo envuelva), no de una dirección de
//! par individual.
//!
//! IMPORTANTE: la dirección del pool_manager en config.example.toml está
//! INCOMPLETA/TRUNCADA (0x8366a39c...) tal como se encontró en la fuente
//! original — hay que completarla y verificarla con `chain::verify` antes
//! de que este módulo pueda hacer nada real.

use crate::trading::{SwapRequest, SwapResult};

pub struct UniswapV4Engine {
    pub pool_manager: String,
}

impl UniswapV4Engine {
    pub fn new(pool_manager: String) -> Self {
        Self { pool_manager }
    }

    /// Identifica si un pool pertenece a un launchpad concreto a partir de
    /// su hook (ej. el hook de Pons: 0xe5e702641ea86f4ae6cc3cdaed2b886f976be044).
    /// Útil para que la UI muestre "graduado de Pons" en vez de solo "pool V4".
    pub fn identify_launchpad_by_hook(&self, hook_address: &str) -> Option<&'static str> {
        // Comparación case-insensitive pendiente de implementar correctamente
        // (direcciones EVM no son case-sensitive salvo checksum EIP-55).
        match hook_address.to_lowercase().as_str() {
            "0xe5e702641ea86f4ae6cc3cdaed2b886f976be044" => Some("pons"),
            _ => None,
        }
    }

    pub async fn buy(&self, req: &SwapRequest) -> anyhow::Result<SwapResult> {
        let _preflight = crate::trading::safety::preflight(req).await?;
        todo!("construir swap contra el PoolManager de Uniswap V4, firmar, enviar")
    }

    pub async fn sell(&self, req: &SwapRequest) -> anyhow::Result<SwapResult> {
        let _preflight = crate::trading::safety::preflight(req).await?;
        todo!("construir swap contra el PoolManager de Uniswap V4, firmar, enviar")
    }
}
