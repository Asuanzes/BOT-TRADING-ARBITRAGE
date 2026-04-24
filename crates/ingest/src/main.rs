mod coinalyze;
mod polymarket;
mod binance;
mod bybit;
mod deribit;
mod features;
mod types;
mod writer;

use anyhow::Result;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::info;

const SPOT_WS: &str = "wss://stream.binance.com:9443/ws";
const BYBIT_LINEAR_WS: &str = "wss://stream.bybit.com/v5/public/linear";
const DERIBIT_WS: &str = "wss://www.deribit.com/ws/api/v2";

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider().install_default().expect("failed to install rustls crypto provider");
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))).init();

    let data_dir = PathBuf::from(std::env::var("BTCBOT_DATA_DIR").unwrap_or_else(|_| "/home/btcbot/btcbot/data/raw".into()));
    info!("data dir: {:?}", data_dir);

    let tx_write = writer::spawn(data_dir);
    let (tx_fan, mut rx_fan) = mpsc::channel::<types::RawEvent>(16384);
    let (tx_feat, rx_feat) = mpsc::unbounded_channel();

    let tx_write_fwd = tx_write.clone();
    let tx_feat_fwd = tx_feat.clone();
    tokio::spawn(async move {
        while let Some(ev) = rx_fan.recv().await {
            let _ = tx_feat_fwd.send(ev.clone());
            let _ = tx_write_fwd.send(ev).await;
        }
    });

    features::spawn(rx_feat);
    info!("btcbot-ingest starting (spot + bybit + deribit)");

    let spot_streams = vec!["btcusdt@aggTrade".to_string(), "btcusdt@depth20@100ms".to_string()];
    let bybit_topics = vec!["publicTrade.BTCUSDT".to_string(), "orderbook.50.BTCUSDT".to_string(), "tickers.BTCUSDT".to_string()];
    let deribit_channels = vec!["ticker.BTC-PERPETUAL.100ms".to_string(), "book.BTC-PERPETUAL.10.100ms".to_string()];

    tokio::spawn(binance::run("binance_spot", SPOT_WS.to_string(), spot_streams, tx_fan.clone()));
    tokio::spawn(bybit::run("bybit_linear", BYBIT_LINEAR_WS.to_string(), bybit_topics, tx_fan.clone()));
    tokio::spawn(deribit::run("deribit_perp", DERIBIT_WS.to_string(), deribit_channels, tx_fan.clone()));

    polymarket::spawn(tx_fan.clone());
    coinalyze::spawn("c2a87d9d-d2a9-4986-807d-90574eb5b055", tx_fan.clone());

    tokio::signal::ctrl_c().await?;
    info!("Shutting down gracefully...");
    drop(tx_fan);
    drop(tx_write);

    Ok(())
}
