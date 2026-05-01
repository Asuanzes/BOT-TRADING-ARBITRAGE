//! Pequeño binario para validar la conexión a Chainlink Data Streams.
//!
//! Uso:
//!     CHAINLINK_API_KEY=...  CHAINLINK_API_SECRET=...  \
//!     CHAINLINK_BTC_USD_FEED_ID=0x...  \
//!     cargo run -p chainlink --example probe
//!
//! Imprime `benchmark_price`, `bid` y `ask`. Si la respuesta llega pero no se
//! decodifica, conviene activar `RUST_LOG=debug` para ver las claves del JSON.

use chainlink::ChainlinkClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,chainlink=debug")),
        )
        .init();

    let client = ChainlinkClient::from_env()?;
    let p = client.get_btc_price_now().await?;
    tracing::info!(
        "chainlink BTC/USD: benchmark={:.2} bid={:?} ask={:?} ts_ns={}",
        p.benchmark_price, p.bid, p.ask, p.ts_ns
    );
    Ok(())
}
