//! Buscador por-token: dada una dirección pegada por el usuario, resuelve el
//! estado actual del token **bajo demanda**, sin suscripciones ni base de
//! datos. Es el punto 1 de la Fase 1 y la funcionalidad que el usuario quiere
//! usar primero.
//!
//! Solo hace `eth_call` (barato y rápido, va por el provider principal). El
//! histórico de eventos para el gráfico y los holders vive en
//! `data::backfill`, que es lento y va por el RPC de logs.
//!
//! Las dos rutas de precio, ambas validadas contra la chain el 2026-09-14:
//!
//! - **En curva:** la curva de Pons V2 es un AMM de producto constante con
//!   liquidez virtual, así que el precio spot es exactamente
//!   `quoteReserve / tokenReserve` de `getReserves()`. No usar
//!   `realQuoteReserve()` para el precio: ese excluye la liquidez virtual y
//!   da un precio equivocado al principio de la curva. Sí se usa para el
//!   progreso hacia la graduación.
//! - **Graduado:** se reconstruye el `PoolKey`, se deriva el `PoolId` y se
//!   lee `getSlot0` de StateView. El precio sale de `sqrtPriceX96`, y hay que
//!   invertirlo cuando el memecoin es `currency1` (caso GOATETO, par ETH
//!   nativo). Ver CLAUDE.md, "Cómo derivar el PoolId y el precio".

use crate::app::state::{TokenPhase, TokenView};
use crate::chain::ChainProvider;
use crate::data::abi::{IERC20Meta, IPonsV2Curve, IPonsV2Factory, IStateView, PoolKey};
use alloy::primitives::aliases::{I24, U24};
use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::sol_types::SolValue;

/// Direcciones que el buscador necesita, ya parseadas. Salen de `config.toml`
/// y **todas han pasado por `chain::verify`** (`cargo run -- verify`): la
/// factory y el hook de Pons, y el PoolManager y StateView de Uniswap V4.
#[derive(Debug, Clone)]
pub struct LookupAddresses {
    pub factory: Address,
    pub hook: Address,
    pub state_view: Address,
    pub pool_manager: Address,
}

impl LookupAddresses {
    /// Toma las direcciones del launchpad Pons habilitado en config.
    pub fn from_config(cfg: &crate::config::AppConfig) -> anyhow::Result<Self> {
        let pons = cfg
            .launchpads
            .iter()
            .find(|l| l.enabled && l.name == "pons")
            .ok_or_else(|| anyhow::anyhow!("no hay ningún launchpad 'pons' habilitado en config.toml"))?;
        Ok(Self {
            factory: pons.launch_factory.parse()?,
            hook: pons.graduation_hook.parse()?,
            state_view: cfg.uniswap_v4.state_view.parse()?,
            pool_manager: cfg.uniswap_v4.pool_manager.parse()?,
        })
    }
}

/// Error de negocio del buscador, separado de los fallos de RPC para que la
/// UI pueda distinguir "este token no es de Pons V2" de "el RPC falló".
#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("{address} no es un token lanzado por la factory de Pons V2 (getLaunchedToken.exists = false). \
             Si es un token de Pons V1, como el token de referencia PONS, su pool es Uniswap V3 y este \
             buscador no lo cubre")]
    NotAPonsV2Launch { address: String },
    #[error("la fase on-chain es {phase} ({label}): no hay ni curva operativa ni pool V4 del que leer precio")]
    NoTradeableVenue { phase: u8, label: &'static str },
}

/// Consulta el estado on-chain de un token. No toca logs ni disco.
pub async fn lookup(
    provider: &ChainProvider,
    addrs: &LookupAddresses,
    token: Address,
) -> anyhow::Result<TokenView> {
    let http = provider.http();
    let factory = IPonsV2Factory::new(addrs.factory, http);

    let launch = factory
        .getLaunchedToken(token)
        .call()
        .await
        .map_err(|e| anyhow::anyhow!("getLaunchedToken({token}) falló: {e}"))?;

    if !launch.exists {
        return Err(LookupError::NotAPonsV2Launch { address: token.to_string() }.into());
    }

    let phase = TokenPhase::from_onchain(launch.phase);

    // Metadata del token. symbol() es el único que puede fallar de forma
    // benigna (algún token exótico lo declara bytes32): no aborta la consulta.
    let erc20 = IERC20Meta::new(token, http);
    let decimals = erc20
        .decimals()
        .call()
        .await
        .map_err(|e| anyhow::anyhow!("decimals() de {token} falló: {e}"))?;
    let symbol = erc20.symbol().call().await.ok();
    let total_supply_raw = erc20
        .totalSupply()
        .call()
        .await
        .map_err(|e| anyhow::anyhow!("totalSupply() de {token} falló: {e}"))?;
    let total_supply = scaled(total_supply_raw, decimals);

    // El pairToken puede ser ETH nativo (address cero) o un ERC-20 cualquiera
    // (stock token, USDG con 6 decimales...). Nunca asumir 18.
    let (pair_symbol, pair_decimals) = if launch.pairToken == Address::ZERO {
        (Some("ETH".to_string()), 18u8)
    } else {
        let pair = IERC20Meta::new(launch.pairToken, http);
        let d = pair
            .decimals()
            .call()
            .await
            .map_err(|e| anyhow::anyhow!("decimals() del pairToken {} falló: {e}", launch.pairToken))?;
        (pair.symbol().call().await.ok(), d)
    };

    let mut view = TokenView {
        address: token.to_string(),
        symbol,
        launchpad: Some("pons".to_string()),
        phase,
        honeypot_checked: false,
        honeypot_risk: None,
        decimals: Some(decimals),
        total_supply: Some(total_supply),
        pair_address: Some(launch.pairToken.to_string()),
        pair_symbol,
        pair_decimals: Some(pair_decimals),
        curve_address: Some(launch.curve.to_string()),
        ..Default::default()
    };

    match phase {
        TokenPhase::BondingCurve => {
            let curve = IPonsV2Curve::new(launch.curve, http);
            let reserves = curve
                .getReserves()
                .call()
                .await
                .map_err(|e| anyhow::anyhow!("getReserves() de la curva {} falló: {e}", launch.curve))?;

            // Producto constante: el precio spot es el cociente de reservas.
            // quoteReserve incluye la liquidez virtual, que es justo lo que
            // hace que haya precio desde el primer token vendido.
            view.price_in_pair = price_from_reserves(
                reserves.quoteReserve,
                reserves.tokenReserve,
                decimals,
                pair_decimals,
            );

            // El progreso sí va contra la quote REAL aportada, no contra la
            // virtual: el umbral de graduación se mide sobre dinero de verdad.
            let real_quote = curve.realQuoteReserve().call().await.ok();
            if let (Some(real), threshold) = (real_quote, launch.graduationThreshold) {
                if !threshold.is_zero() {
                    view.graduation_progress =
                        Some((u256_to_f64(real) / u256_to_f64(threshold)).clamp(0.0, 1.0));
                }
            }
        }
        TokenPhase::Graduated => {
            let (pool_id, memecoin_is_currency0) =
                derive_pool_id(token, launch.pairToken, launch.poolFee, launch.tickSpacing, addrs.hook);
            view.pool_id = Some(format!("{pool_id:#x}"));

            let sv = IStateView::new(addrs.state_view, http);
            let slot0 = sv
                .getSlot0(pool_id)
                .call()
                .await
                .map_err(|e| anyhow::anyhow!("getSlot0({pool_id:#x}) falló: {e}"))?;
            view.price_in_pair = Some(price_from_sqrt_price_x96(
                U256::from(slot0.sqrtPriceX96),
                memecoin_is_currency0,
                decimals,
                pair_decimals,
            ));
            view.pool_liquidity = sv.getLiquidity(pool_id).call().await.ok();
        }
        TokenPhase::Swept | TokenPhase::Rescued | TokenPhase::Unknown => {
            // Swept es un estado transitorio dentro de la graduación y
            // Rescued una graduación fallida: en ninguno de los dos hay un
            // sitio del que leer precio. Se devuelve la metadata igualmente.
            tracing::warn!(
                token = %token, phase = launch.phase,
                "el token no está ni en curva ni graduado; sin precio"
            );
        }
    }

    if let (Some(price), Some(supply)) = (view.price_in_pair, view.total_supply) {
        view.market_cap_in_pair = Some(price * supply);
    }

    Ok(view)
}

/// `PoolId = keccak256(abi.encode(PoolKey))`. Devuelve también si el memecoin
/// quedó como `currency0`, que decide si el precio de `sqrtPriceX96` hay que
/// invertirlo.
///
/// El orden de monedas es el de Uniswap V4: la dirección numéricamente menor
/// es `currency0`. ETH nativo es la dirección cero, así que siempre es
/// `currency0` y el memecoin queda de `currency1` (caso GOATETO).
pub fn derive_pool_id(
    token: Address,
    pair_token: Address,
    pool_fee: U24,
    tick_spacing: I24,
    hook: Address,
) -> (B256, bool) {
    let memecoin_is_currency0 = token < pair_token;
    let (currency0, currency1) = if memecoin_is_currency0 {
        (token, pair_token)
    } else {
        (pair_token, token)
    };
    let key = PoolKey {
        currency0,
        currency1,
        fee: pool_fee,
        tickSpacing: tick_spacing,
        hooks: hook,
    };
    (keccak256(key.abi_encode()), memecoin_is_currency0)
}

/// Precio de 1 token en unidades de pairToken, a partir de `sqrtPriceX96`.
///
/// `(sqrtPriceX96 / 2^96)^2` es el precio crudo de currency1 por currency0.
/// Se invierte si el memecoin es currency1, y se reescala por la diferencia
/// de decimales entre token y par.
pub fn price_from_sqrt_price_x96(
    sqrt_price_x96: U256,
    memecoin_is_currency0: bool,
    token_decimals: u8,
    pair_decimals: u8,
) -> f64 {
    let ratio = {
        let s = u256_to_f64(sqrt_price_x96) / 2f64.powi(96);
        s * s
    };
    let raw = if memecoin_is_currency0 { ratio } else { 1.0 / ratio };
    raw * decimal_scale(token_decimals, pair_decimals)
}

/// Precio spot de un AMM de producto constante: cociente de reservas,
/// reescalado por decimales. `None` si la reserva de tokens es cero.
pub fn price_from_reserves(
    quote_reserve: U256,
    token_reserve: U256,
    token_decimals: u8,
    pair_decimals: u8,
) -> Option<f64> {
    if token_reserve.is_zero() {
        return None;
    }
    let raw = u256_to_f64(quote_reserve) / u256_to_f64(token_reserve);
    Some(raw * decimal_scale(token_decimals, pair_decimals))
}

/// `10^(token_decimals - pair_decimals)`, el factor que convierte un cociente
/// de importes crudos en precio entre unidades humanas.
pub fn decimal_scale(token_decimals: u8, pair_decimals: u8) -> f64 {
    10f64.powi(token_decimals as i32 - pair_decimals as i32)
}

/// U256 a f64. Pasa por decimal en texto porque `U256` no tiene conversión
/// directa a f64 y los valores aquí (precios, supplies) caben de sobra en la
/// precisión de f64 para lo que se muestra.
pub fn u256_to_f64(v: U256) -> f64 {
    v.to_string().parse::<f64>().unwrap_or(f64::NAN)
}

/// Importe crudo a unidades humanas.
pub fn scaled(raw: U256, decimals: u8) -> f64 {
    u256_to_f64(raw) / 10f64.powi(decimals as i32)
}
