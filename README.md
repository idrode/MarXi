# robinhood-sniper (nombre provisional)

TUI self-custodial en Rust para trading/sniping de memecoins y tokens en
**Robinhood Chain** (chain ID `4663`, L2 Arbitrum Orbit), operando sobre
launchpads tipo Pons (bonding curve → graduación a Uniswap V4).

Sin dependencia de infraestructura de terceros para custodiar tu clave: la
clave privada vive cifrada en disco (keystore JSON tipo Ethereum) y
descifrada solo en memoria durante la sesión.

> Este proyecto nace como pivote del proyecto original pensado para
> HyperEVM/HyperSwap. La arquitectura general (Rust + ratatui + tokio,
> keystore cifrado, checklist de seguridad antes de firmar) se traslada
> tal cual; lo que cambia es la chain, el DEX (Uniswap V4 en vez de
> HyperSwap) y la existencia de una fase adicional de bonding-curve
> pre-graduación que no existía en el diseño original.

## Estado del proyecto

**Esqueleto inicial.** La estructura de módulos, tipos y firmas de función
están definidos; la lógica real (llamadas RPC, firma de transacciones,
render de UI) está marcada con `todo!()` de forma deliberada — este
código compila en cuanto a tipos pero no ejecuta nada real todavía. Ver
"Plan de desarrollo por fases" abajo para el orden de implementación.

## ⚠️ Riesgos y disclaimer

- **Eres 100% responsable de tu clave privada y de cualquier fondo que
  gestione esta herramienta.** No hay soporte, no hay forma de recuperar
  fondos perdidos por un bug, una mala configuración, o un error de
  operación.
- Los memecoins recién lanzados son, por naturaleza, de altísimo riesgo:
  iliquidez, honeypots, rugs, taxes de compra/venta anómalos. Ninguna
  verificación automática (honeypot check, simulación previa) elimina
  ese riesgo, solo lo reduce.
- Varias direcciones de contrato usadas en `config.example.toml` (Pons
  factory/router/hook, Uniswap V4 pool manager) provienen de
  documentación pública de terceros recopilada en investigación, **no de
  una fuente on-chain verificada por este código todavía**. Antes de
  operar con dinero real, cada una debe pasar por `chain::verify`
  (bytecode desplegado + confirmación de rol) — ver comentarios en ese
  módulo.
- Robinhood Chain y su ecosistema de launchpads llevan pocas semanas
  operando (mainnet: julio 2026). Es razonable esperar cambios de
  contratos, deprecaciones, y comportamientos no documentados con más
  frecuencia que en ecosistemas maduros.
- No es asesoramiento financiero ni legal. Si en algún momento este
  proyecto evoluciona hacia gestionar fondos de otras personas (no es
  el caso de este MVP, pensado para uso individual), eso cambia por
  completo las implicaciones legales y regulatorias — no asumido aquí.

## Instalación

```bash
git clone <este-repo>
cd robinhood-sniper
cp config.example.toml config.toml
cp .env.example .env
# editar config.toml y .env con tus valores (API keys de RPC, etc.)
cargo build --release
```

Requiere Rust estable reciente (edición 2021; se evaluará mover a 2024
cuando el toolchain lo soporte de forma consistente en el entorno de
desarrollo).

## Configuración

- `config.toml`: parámetros no sensibles (chain, slippage por defecto,
  launchpads habilitados, rutas de archivos). Ver `config.example.toml`
  para la referencia comentada.
- `.env`: API keys de proveedores RPC (Alchemy recomendado — el RPC
  público oficial de Robinhood Chain está rate-limited y no pensado para
  tráfico de producción según su propia documentación) y de proveedor de
  honeypot-check.
- La clave privada de trading **no vive en ningún archivo de config**:
  se gestiona vía keystore cifrado (`security::keystore`), generado o
  importado la primera vez que arrancas la app, desbloqueado con
  contraseña interactiva en cada sesión.

## Arquitectura

```
src/
  main.rs            — entry point, orden de arranque
  app/                — estado global de la TUI + bus de eventos (AppEvent)
  chain/              — provider alloy (HTTP+WS) y verificación on-chain de contratos
  data/                — indexador: watcher de eventos, agregación de velas, metadata, SQLite
  trading/            — motor dual: curve_engine (pre-graduación) + v4_engine (post-graduación),
                          checklist de seguridad (safety.rs), gestión de posiciones
  security/           — keystore cifrado, honeypot check
  ui/                  — paneles ratatui: dashboard, sniper, token_detail, positions, settings
  config/              — carga/validación de config.toml
```

Patrón de concurrencia: cada tarea de fondo (listener de eventos on-chain,
indexador) corre en su propio `tokio::spawn` y comunica con el loop de
render exclusivamente vía un canal `mpsc` de `AppEvent`. El loop de
render nunca bloquea en I/O de red o disco.

### Por qué dos motores de trading

En los launchpads de Robinhood Chain (Pons y similares), un token recién
lanzado no tiene un pool de Uniswap desde el minuto uno: se tradea contra
una **bonding curve** propia del token hasta que acumula suficiente
depósito para "graduar", momento en el que la liquidez se siembra
permanentemente en un pool de **Uniswap V4**. Comprar/vender antes y
después de ese punto son interacciones con contratos completamente
distintos — de ahí `trading::curve_engine` y `trading::v4_engine` como
módulos separados, en vez de una abstracción prematura sobre ambos.

## Riesgos de seguridad y mitigaciones

| Riesgo | Mitigación |
|---|---|
| Clave privada expuesta | Keystore cifrado, solo en memoria durante sesión, `zeroize` al soltar |
| Transacción revierte y se pierde gas | Simulación previa obligatoria (`trading::safety::preflight`) |
| Slippage descontrolado | Límite configurable, default conservador (3%) |
| `approve` infinito explotable | Approve acotado al monto exacto por defecto |
| Transacción colgada en mempool a precio desfasado | Deadline en cada transacción |
| Comprar un honeypot | Chequeo vía proveedor externo (fase graduada) + heurística propia (fase curve) — ninguno de los dos es garantía absoluta |
| Dirección de contrato incorrecta/placeholder | Verificación on-chain obligatoria antes de primer uso (`chain::verify`) |
| Latencia alta en el lane equivocado | *(heredado del diseño original en HyperEVM — Robinhood Chain no tiene dual-block-lane conocido; pendiente confirmar si aplica algún mecanismo equivalente en Arbitrum Orbit)* |

## Plan de desarrollo por fases

**Fase 0 — Verificación de infraestructura (antes de escribir lógica real)**
- Confirmar RPC (Alchemy) funcionando contra chain 4663
- Verificar on-chain las direcciones de Pons (factory/router/hook) y el
  pool manager completo de Uniswap V4 (la dirección encontrada en
  investigación estaba truncada)
- Confirmar si GoPlus (u otro proveedor) soporta esta chain vía API

**Fase 1 — Indexador de solo-lectura**
- Provider alloy real (HTTP + WS)
- Watcher de eventos del factory de Pons (`TokenLaunched`, `LaunchSwept`/`PoolGraduated`)
- Agregación de velas para un token seguido manualmente
- Persistencia SQLite
- UI: dashboard + sniper feed, sin trading todavía

**Fase 2 — Trading**
- Keystore: generación/importación + desbloqueo interactivo
- `curve_engine` para Pons: buy/sell reales, con `safety::preflight` completo
- `v4_engine`: swap post-graduación
- UI: panel de token detail con acción de compra/venta y confirmación explícita

**Fase 3 — Gestión de posiciones y TP/SL**
- `PositionManager` con cierre automático al cumplirse TP/SL (pasando
  siempre por el mismo preflight que una venta manual)
- PnL en vivo en el panel de posiciones

**Fase 4 — Indicadores propios**
- Portar el framework de indicadores ya construido para trading de perps
  (BB + RSI/ADX/DMI + histograma de whale-signal) adaptado a memecoins:
  la señal de "whale" pasa a construirse desde `Transfer`/holders en vez
  de OI/funding
- Renderizado en el mismo panel que el gráfico de velas, reutilizando el
  patrón visual ya resuelto en el proyecto hermano `hyperT`

**Fase 5 — Soporte multi-launchpad**
- Añadir Bankr, Long, Pools.trade como adaptadores adicionales de
  `curve_engine` (cada uno con su propio formato de curva/eventos — no
  asumir que comparten ABI con Pons)

**No en el roadmap actual (fuera de alcance explícito):**
- Copy-trading de wallets específicas — mencionado como posible interés
  en el prompt inicial de este proyecto, pero no forma parte del MVP ni
  de las fases planificadas todavía; requiere su propio diseño (qué
  wallets seguir, cómo replicar tamaño de posición, límites de riesgo)
- Soporte hardware wallet (Ledger) — la ruta de gestión de claves actual
  es keystore cifrado en software; hardware wallet queda como exploración
  futura, no comprometida en ninguna fase
- Cualquier variante multiusuario/custodial — este proyecto es,
  deliberadamente, para uso individual del propio operador
