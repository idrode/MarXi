//! Interfaces y eventos `sol!` de Pons V2 y Uniswap V4, en un solo sitio para
//! que `token_lookup` y `backfill` no dupliquen firmas.
//!
//! **Ninguna firma de aquí es inventada.** Todas salen del fuente oficial
//! (`ponsdotdev/ponsfamily/contractsV2/src/v2/` y
//! `Uniswap/v4-core/src/interfaces/IPoolManager.sol`), y sus topic0 están
//! recomputados con keccak y contrastados contra logs reales de la chain el
//! 2026-09-14. La tabla completa de topic0 está en CLAUDE.md, sección
//! "Paso 0". Si hay que tocar algo aquí, volver al fuente, no a Bitquery ni
//! a un explorer.
//!
//! Detalle que sorprende si se viene de Uniswap V2/V3: `PoolId` es un
//! `bytes32` y el `Swap` de V4 es **un único evento del PoolManager
//! singleton** para todos los pools, no un evento por par.

use alloy::sol;

sol! {
    // ---------------------------------------------------------------
    // Uniswap V4 — v4-core/src/interfaces/IPoolManager.sol
    // ---------------------------------------------------------------
    #[derive(Debug)]
    event Swap(
        bytes32 indexed id,
        address indexed sender,
        int128 amount0,
        int128 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick,
        uint24 fee
    );

    #[derive(Debug)]
    event Initialize(
        bytes32 indexed id,
        address indexed currency0,
        address indexed currency1,
        uint24 fee,
        int24 tickSpacing,
        address hooks,
        uint160 sqrtPriceX96,
        int24 tick
    );

    // ---------------------------------------------------------------
    // Pons V2 — factory (PonsV2LaunchFactory.sol)
    // ---------------------------------------------------------------
    #[derive(Debug)]
    event TokenLaunched(
        address indexed token,
        address indexed curve,
        address indexed deployer,
        address pairToken,
        uint256 launchConfigId,
        uint256 graduationThreshold
    );

    #[derive(Debug)]
    event PoolGraduated(
        address indexed token,
        uint256 positionId,
        uint256 tokenAmount,
        uint256 pairTokenAmount
    );

    // ---------------------------------------------------------------
    // Pons V2 — bonding curve (PonsV2BondingCurve.sol)
    // ---------------------------------------------------------------
    #[derive(Debug)]
    event CurveBuy(
        address indexed buyer,
        address indexed recipient,
        uint256 quoteIn,
        uint256 tokensOut,
        uint256 fee,
        uint256 tax
    );

    #[derive(Debug)]
    event CurveSell(
        address indexed seller,
        address indexed recipient,
        uint256 tokensIn,
        uint256 quoteOut,
        uint256 fee,
        uint256 tax
    );

    // ---------------------------------------------------------------
    // ERC-20 estándar
    // ---------------------------------------------------------------
    #[derive(Debug)]
    event Transfer(address indexed from, address indexed to, uint256 value);

    // ---------------------------------------------------------------
    // Structs
    // ---------------------------------------------------------------

    /// Clave de pool de Uniswap V4. `PoolId = keccak256(abi.encode(PoolKey))`,
    /// cinco palabras de 32 bytes. La factory de Pons lo reconstruye igual en
    /// `_poolIdFor`, no lo almacena.
    struct PoolKey {
        address currency0;
        address currency1;
        uint24 fee;
        int24 tickSpacing;
        address hooks;
    }

    /// `interfaces/ILaunchpadV2.sol`. El orden de campos importa para
    /// decodificar: es el que devuelve `getLaunchedToken`.
    struct LaunchedToken {
        address token;
        address curve;
        address deployer;
        address creatorFeeRecipient;
        address pairToken;
        uint256 graduationThreshold;
        uint24 poolFee;
        int24 tickSpacing;
        uint16 creatorTaxBps;
        bool buybackEnabled;
        /// enum GraduationPhase: 0 NotGraduated, 1 Swept, 2 PoolCreated, 3 Rescued
        uint8 phase;
        uint256 sweptQuote;
        uint256 sweptTokens;
        uint256 sweptAt;
        bool exists;
    }

    // ---------------------------------------------------------------
    // Interfaces de lectura
    // ---------------------------------------------------------------

    #[sol(rpc)]
    interface IPonsV2Factory {
        function getLaunchedToken(address token) external view returns (LaunchedToken memory);
    }

    /// Curva propia de cada token. `getReserves` incluye la liquidez virtual
    /// (`phantomQuote`), que es la que define el precio; `realQuoteReserve`
    /// es solo lo físicamente depositado y sirve para el progreso hacia la
    /// graduación. Ver CLAUDE.md, "Precio en fase bonding-curve".
    #[sol(rpc)]
    interface IPonsV2Curve {
        function getReserves() external view returns (uint256 quoteReserve, uint256 tokenReserve);
        function realQuoteReserve() external view returns (uint256);
        function graduationThreshold() external view returns (uint256);
        function graduated() external view returns (bool);
        function readyToGraduate() external view returns (bool);
    }

    /// Lens de solo lectura de Uniswap V4 (v4-periphery StateView).
    #[sol(rpc)]
    interface IStateView {
        function poolManager() external view returns (address);
        function getSlot0(bytes32 poolId)
            external
            view
            returns (uint160 sqrtPriceX96, int24 tick, uint24 protocolFee, uint24 lpFee);
        function getLiquidity(bytes32 poolId) external view returns (uint128);
    }

    #[sol(rpc)]
    interface IERC20Meta {
        function symbol() external view returns (string);
        function decimals() external view returns (uint8);
        function totalSupply() external view returns (uint256);
    }
}

// -------------------------------------------------------------------
// Filtros derivados de los eventos de arriba
// -------------------------------------------------------------------
//
// Viven aquí y no en `operator_tracker` porque dependen del orden exacto
// de los campos `indexed` de `TokenLaunched`, que es lo que este módulo
// documenta. Si la firma cambia, el filtro se rompe en el mismo archivo.

use alloy::primitives::{Address, Log as PrimLog};
use alloy::rpc::types::{Filter, Log};
use alloy::sol_types::SolEvent;

/// Bloque en el que arranca la factory V2 activa (`0x7ed598…`). Por debajo
/// de aquí no hay ningún `TokenLaunched` suyo. Verificado el 2026-09-14
/// (la legacy arranca en 8600612 y no se usa).
pub const FACTORY_START_BLOCK: u64 = 8_991_118;

/// Filtro de `TokenLaunched` de un deployer concreto.
///
/// `deployer` es el **tercer** campo `indexed` del evento, así que va en
/// `topics[3]`; `topics[1]` es el token y `topics[2]` la curva. Filtrar en
/// el propio RPC es lo que hace barato el historial de un operador: el nodo
/// devuelve solo sus lanzamientos, no los ~333 por cada 20 000 bloques que
/// tiene el launchpad entero.
///
/// Sin rango de bloques: lo pone `ChainProvider::get_logs_backfill`, que es
/// quien trocea y reintenta (el RPC público da timeout en rangos de decenas
/// de millones de bloques).
pub fn token_launched_by_deployer_filter(factory: Address, deployer: Address) -> Filter {
    Filter::new()
        .address(factory)
        .event_signature(TokenLaunched::SIGNATURE_HASH)
        .topic3(deployer.into_word())
}

/// Filtro de todos los `TokenLaunched` de la factory, sin filtrar deployer.
/// Lo usa el watcher del factory completo (Fase 1, punto 2).
pub fn token_launched_filter(factory: Address) -> Filter {
    Filter::new().address(factory).event_signature(TokenLaunched::SIGNATURE_HASH)
}

/// Decodifica un log como `TokenLaunched`.
///
/// Devuelve error en vez de descartar en silencio: un log que llegó por un
/// filtro con este topic0 y no decodifica significa que la firma de aquí ya
/// no coincide con la de la chain, y eso hay que verlo, no tragarlo. Es la
/// misma lección del backfill que perdía dos tercios de los trades.
pub fn decode_token_launched(log: &Log) -> anyhow::Result<PrimLog<TokenLaunched>> {
    TokenLaunched::decode_log(log.as_ref())
        .map_err(|e| anyhow::anyhow!("log en {:?} no decodifica como TokenLaunched: {e}", log.block_number))
}
