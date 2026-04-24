# btcbot (Rust, VPS Hetzner)

## Stack y propósito
- Lenguaje: Rust con Cargo workspace.
- Proyecto: bot de trading automatizado.
- Fuentes de datos actuales:
  - Binance
  - Coinalyze
  - Deribit
  - Polymarket
- Objetivo: automatizar trading en Polymarket en mercados de corta duración (p.ej. 5 minutos),
  tomando decisiones sobre:
  - Si apostar a up/down respecto al precio de referencia.
  - Tipo de orden (market vs limit; de momento preferir una sola opción simple).
  - Momento de entrada dentro de la ventana de 5 minutos.
  - Cuándo recoger ganancias o cortar pérdidas en función de la evolución del precio.

## Estado del proyecto
- El bot ya está corriendo en el servidor y lleva bastantes horas de ejecución.
- La autenticación (API keys) aún no está completamente configurada; esto se hará más adelante.
- Falta estructurar la lógica de decisión de trading y desarrollarla de forma robusta.

## Estructura general esperada
- `Cargo.toml` (raíz): define el workspace de Rust.
- `src/`: coordinación de alto nivel (entrypoint del bot, orquestación general).
- `crates/ingest/`: ingesta de datos (lectura de datos de exchanges / mercados).
- Otros crates o módulos podrán añadirse para:
  - Lógica de decisión (estrategias).
  - Gestión de riesgo y tamaño de posición.
  - Integración específica con Polymarket (órdenes, gestión de posiciones).

## Comandos habituales en este servidor
- `cargo build --release` — Compilar para producción.
- `cargo test` — Ejecutar tests.
- `./target/release/btcbot` — Ejecutar binario manualmente (solo si procede).
- `systemctl restart btcbot` — Reiniciar servicio del bot (si existe unidad systemd).
- `journalctl -u btcbot -f` — Ver logs del servicio en tiempo real (si hay servicio).

## Reglas importantes para Claude Code (token y seguridad)
- Usa **comandos de shell primero** (`ls`, `tree`, `rg`, `grep`, `cargo`) para entender el proyecto
  antes de leer archivos grandes.
- No abras archivos de más de ~500 líneas sin pedírmelo explícitamente.
- Prioriza trabajar con:
  - `src/`
  - `crates/ingest/src/`
  y otros módulos que yo te indique, en lugar de leer todo el repo.
- Nunca muestres ni copies API keys, tokens o credenciales desde `.env`, `config/` u otros archivos
  de configuración. Si necesitas usarlas, asume que vendrán de variables de entorno o de un módulo
  seguro, sin imprimirlas.
- Haz cambios **pequeños y bien justificados**:
  - Primero, propón un plan en varios pasos.
  - Después, aplica cambios mínimos (por ejemplo, en una sola función o archivo) y explícame el diff.
- No reescribas archivos enteros si no es estrictamente necesario; trabaja de forma incremental:
  añade funciones, refactores locales y tests donde tenga sentido.

## Guía para la lógica de trading (alto nivel, aún por implementar)
- Caso principal: mercados de 5 minutos en Polymarket.
- Preguntas que debe responder la lógica:
  1. ¿Se espera que el precio suba o baje respecto al precio de referencia?
  2. ¿Qué tipo de orden usar? (por simplicidad inicial, se puede empezar solo con market o solo con limit).
  3. ¿En qué momento de la ventana de 5 minutos entrar?
  4. ¿Cuándo recoger beneficios?
     - Ejemplo: si se compra a 0.35, recoger ganancias con +50%.
  5. ¿Cuándo dejar correr las ganancias si se ve un "up" o "down" claro? diferencia con el beat Price >50$ por ejemplo, ya sea up o down
     - Ejemplo: monitorizar segundo a segundo y cerrar si hay una reversión.
  6. ¿Cuándo cortar pérdidas?
     - Basado en diferencias con el precio de referencia (umbrales X arriba/abajo).

- Al implementar, preferimos:
  - Empezar con una **estrategia simple y explícita**, fácilmente testeable.
  - Evitar sobre-optimizar o complicar demasiado el tipo de orden al principio.
  - Dejar bien separado:
    - la ingesta de datos;
    - la lógica de decisión;
    - la capa de ejecución de órdenes en Polymarket.
