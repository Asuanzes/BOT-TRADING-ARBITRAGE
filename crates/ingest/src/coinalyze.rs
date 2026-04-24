use crate::types::RawEvent;
use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{error, info};

const BASE: &str = "https://api.coinalyze.net/v1";
const SYMBOLS: &str = "BTCUSDT_PERP.6,BTCUSDT_PERP.A";

pub fn spawn(api_key: &'static str, tx: mpsc::Sender<RawEvent>) {
    tokio::spawn(async move {
        coinalyze_loop(api_key, tx).await;
    });
}

async fn coinalyze_loop(api_key: &str, tx: mpsc::Sender<RawEvent>) {
    info!("coinalyze: starting REST polling (60s)");
    let client = reqwest::Client::new();
    let mut tick = interval(Duration::from_secs(60));

    loop {
        tick.tick().await;

        let endpoints = [
            ("open_interest",    format!("{BASE}/open-interest?symbols={SYMBOLS}&api_key={api_key}")),
            ("funding_rate",     format!("{BASE}/funding-rate?symbols={SYMBOLS}&api_key={api_key}")),
            ("long_short_ratio", format!("{BASE}/long-short-ratio?symbols={SYMBOLS}&api_key={api_key}")),
            ("liquidation",      format!("{BASE}/liquidation-history?symbols={SYMBOLS}&api_key={api_key}")),
        ];

        for (name, url) in &endpoints {
            match client.get(url).send().await {
                Ok(resp) => match resp.json::<serde_json::Value>().await {
                    Ok(payload) => {
                        let ev = RawEvent {
                            feed: "coinalyze",
                            stream: name.to_string(),
                            ts_recv_ns: Utc::now().timestamp_nanos_opt().unwrap_or(0),
                            payload,
                        };
                        if tx.send(ev).await.is_err() {
                            error!("coinalyze: channel closed");
                            return;
                        }
                        info!("coinalyze: {} OK", name);
                    }
                    Err(e) => error!("coinalyze: {} parse error: {e}", name),
                },
                Err(e) => error!("coinalyze: {} request error: {e}", name),
            }
        }
    }
}
