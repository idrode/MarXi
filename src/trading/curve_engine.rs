//! Motor de compra/venta contra la bonding curve de un token recién
//! lanzado (pre-graduación). Cada launchpad tiene su propio formato de
//! curva — este módulo empieza soportando solo Pons V2, tal como se
//! decidió: un adaptador por launchpad, no una abstracción prematura sobre
//! los tres a la vez.
//!
//! Verificado en investigación (Bitquery docs, ago-2026): en Pons, cada
//! token lanzado tiene su PROPIO contrato de curva dedicado (no un
//! contrato compartido) — así que "la dirección de la curva" no es fija en
//! config, se obtiene a partir del evento TokenLaunched o consultando el
//! factory por la dirección del token.
//!
//! Este motor NUNCA construye una tx sin pasar antes por
//! `trading::safety::preflight`.

use crate::trading::{SwapRequest, SwapResult};

pub struct PonsCurveEngine {
    pub launch_factory: String,
    pub launch_router: String,
}

impl PonsCurveEngine {
    pub fn new(launch_factory: String, launch_router: String) -> Self {
        Self { launch_factory, launch_router }
    }

    /// Dado un token en fase curve, resuelve la dirección de su contrato de
    /// curva dedicado. Necesario antes de poder simular o ejecutar nada.
    pub async fn resolve_curve_contract(&self, _token_address: &str) -> anyhow::Result<String> {
        todo!(
            "consultar el factory (o el evento TokenLaunched cacheado por el \
             indexador) para obtener la dirección de la curva de este token"
        )
    }

    /// Compra contra la curva (CurveBuy). NO es un swap de Uniswap — es una
    /// interacción directa con el contrato de curva.
    pub async fn buy(&self, req: &SwapRequest) -> anyhow::Result<SwapResult> {
        let _preflight = crate::trading::safety::preflight(req).await?;
        todo!("construir calldata de compra contra la curva, firmar, enviar")
    }

    pub async fn sell(&self, req: &SwapRequest) -> anyhow::Result<SwapResult> {
        let _preflight = crate::trading::safety::preflight(req).await?;
        todo!("construir calldata de venta contra la curva, firmar, enviar")
    }

    /// Cuánto falta para que el token gradúe (cruce el graduationThreshold).
    /// Útil para la UI: mostrar progreso hacia graduación es justo el tipo
    /// de señal que hace útil el sniping en esta fase.
    pub async fn graduation_progress(&self, _token_address: &str) -> anyhow::Result<f64> {
        todo!("leer cumulative deposits vs graduationThreshold del contrato de curva")
    }
}
