# marXi

Herramienta de terminal en Rust para búsqueda y análisis on-chain en **Robinhood Chain**, centrada en el launchpad **Pons V2**.

**No es un bot de trading.** No ejecuta operaciones, no gestiona claves privadas activamente y no promete señales de entrada/salida. Vigila **wallets que despliegan tokens** ("operadores"), aprende su historial de lanzamientos y financiación, y avisa cuando una wallet vigilada lanza algo nuevo.

100% self-custodial: no depende de ningún servidor de terceros. Corre en local, contra tu propio RPC.

## Qué hace hoy

- **Buscador por-token**: pega una dirección de token y obtén precio, market cap, holders y velas — tanto en fase de curva de bonding como ya graduado a un pool de Uniswap V4.
- **`operator_tracker`** (el núcleo del proyecto):
  - **Historial**: todos los tokens lanzados por una wallet dada, con clasificación de la wallet (EOA normal, delegada EIP-7702, posible contrato-relay).
  - **Financiación**: de dónde salió el capital de arranque de esa wallet, separando financiación externa real de ingresos propios del negocio (ventas en su propia curva) y de ruido de infraestructura (relays de gas).
  - **Watchlist persistente**: una lista local de wallets a vigilar (`watchlist.toml`, nunca se sube al repo).
  - **Vigilancia en vivo**: alertas cuando una wallet de la watchlist lanza un token nuevo (vía WebSocket + evento `TokenLaunched`).
  - **`batch-funding`**: corre el análisis de financiación/creador sobre una lista de tokens de una vez, con salida en CSV, para investigación y contraste de hipótesis a mayor escala.

## Qué NO hace (todavía, o nunca)

- **No ejecuta trades.** Los motores de trading (`curve_engine`, `v4_engine`) están documentados como diseño pero deliberadamente sin implementar — no son el objetivo del proyecto.
- **No custodia clave privada de forma activa.** No hay flujo de firma de transacciones en el camino crítico.
- **No detecta honeypots todavía.**
- **No es un scanner global.** No indexa todo el launchpad en tiempo real ni compite en velocidad/volumen con herramientas como GMGN — el objetivo es vigilar un conjunto pequeño y curado a mano de wallets, no "verlo todo".

## Instalación

```bash
git clone https://github.com/idrode/MarXi.git
cd MarXi/marXi-sniper
cp config.example.toml config.toml   # rellena tus RPC/API keys
cargo build --release
```

## Configuración

- `config.toml` (gitignored): RPC de Alchemy (calls, estado, WebSocket) + RPC público (rango amplio de `eth_getLogs`), direcciones verificadas on-chain de Pons V2 y Uniswap V4 en Robinhood Chain, URL del explorer (Blockscout).
- `watchlist.toml` (gitignored): tu lista local de wallets vigiladas. Nunca se comparte ni se sube.
- `.env` (gitignored): claves/API keys sensibles.

## Comandos principales

```bash
cargo run -- verify                       # verifica todas las direcciones configuradas contra la chain
cargo run -- token <addr> [candle_secs]   # precio/mcap/holders/velas de un token
cargo run -- preview <addr>               # vista rápida de un token
cargo run -- operator <addr>              # historial de lanzamientos de una wallet
cargo run -- funding <addr> [horas] [--from-block N]   # análisis de financiación de arranque
cargo run -- watch list                   # lista la watchlist
cargo run -- watch <addr> [--label][--notes][--force]  # añade una wallet a la watchlist
cargo run -- watch remove <addr>          # quita una wallet de la watchlist
cargo run -- alert list                   # alertas registradas
cargo run -- batch-funding <fichero> [--out][--lookback-blocks]  # análisis por lotes
cargo run                                  # TUI interactiva
```

## Arquitectura

```
src/
├── chain/
│   ├── provider.rs      # doble proveedor: Alchemy (calls/WS) + RPC público (logs de rango amplio)
│   └── verify.rs        # verificación on-chain de cada dirección configurada (bytecode + getter de rol)
├── data/
│   ├── abi.rs            # interfaces/eventos canónicos de Pons V2 y Uniswap V4
│   ├── token_lookup.rs   # resolución de estado y precio de un token (curva o graduado)
│   ├── backfill.rs       # histórico de trades/velas, holders
│   ├── db.rs              # persistencia SQLite (operadores, lanzamientos, financiación, alertas)
│   └── operator_tracker/
│       ├── history.rs     # historial de lanzamientos + clasificación del deployer
│       ├── funding.rs     # financiaciones ERC-20 y nativas
│       ├── baseline.rs    # FundingBaseline: separa financiación de arranque de ingreso/ruido
│       └── watchlist.rs   # watchlist.toml
├── trading/                # curve_engine, v4_engine — diseñado, sin implementar, no prioritario
├── security/               # keystore — diseñado, sin implementar, no prioritario
├── ui/                     # TUI (ratatui): buscador, operador, alertas
├── cli.rs
└── main.rs
```

## Por qué existen dos motores de trading (aunque no se usen)

El diseño original contemplaba dos rutas de ejecución: una para la fase de curva de bonding (`curve_engine`) y otra para el pool ya graduado en Uniswap V4 (`v4_engine`), porque son mecánicas de precio y liquidez distintas. Se mantiene la separación en el código como documentación de diseño por si en el futuro se retoma la ejecución de trades, pero hoy ninguno de los dos está implementado ni es el foco del proyecto.

## Estado del proyecto

En desarrollo activo, uso individual. El núcleo funcional es `operator_tracker`: historial y financiación están medidos y validados contra la chain real; la watchlist y las alertas en vivo están implementadas; el análisis de qué patrones distinguen a un operador que repite éxito del ruido general del mercado sigue en curso.

## Riesgos y disclaimer

- Herramienta de solo lectura frente a la chain: no mueve fondos ni firma transacciones por ti.
- Los datos on-chain se interpretan con hipótesis explícitas, medidas contra la chain real; una correlación observada no implica información privilegiada ni garantía de resultado futuro.
- Robinhood Chain y Pons V2 son relativamente nuevos: las direcciones de contratos y el comportamiento del RPC pueden cambiar; `cargo run -- verify` existe precisamente para no asumir nada sin comprobarlo.
- Este proyecto es un experimento personal de investigación on-chain, no asesoramiento financiero.
