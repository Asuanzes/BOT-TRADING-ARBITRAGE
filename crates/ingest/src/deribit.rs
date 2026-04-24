use crate::types::RawEvent;
use anyhow::Result;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

pub async fn run(
    feed_name: &'static str,
    url: String,
    channels: Vec<String>,
    tx: mpsc::Sender<RawEvent>,
) {
    let mut backoff = 1u64;
    loop {
        match stream_once(feed_name, &url, &channels, &tx).await {
            Ok(_) => warn!("{feed_name}: closed"),
            Err(e) => error!("{feed_name}: error: {e}"),
        }
        info!("{feed_name}: reconnect in {}s", backoff);
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

async fn stream_once(
    feed_name: &'static str,
    url: &str,
    channels: &[String],
    tx: &mpsc::Sender<RawEvent>,
) -> Result<()> {
    let (mut ws, resp) = connect_async(url).await?;
    info!("{feed_name}: connected http {}", resp.status());

    for (i, ch) in channels.iter().enumerate() {
        let sub = serde_json::json!({
            "jsonrpc": "2.0",
            "id": (i + 1) as u64,
            "method": "public/subscribe",
            "params": { "channels": [ch] }
        });
        ws.send(Message::Text(sub.to_string().into())).await?;
    }
    info!("{feed_name}: subscribed to {} channels", channels.len());

    let mut counts: std::collections::HashMap<String, u64> = Default::default();
    let mut last_log = std::time::Instant::now();

    while let Some(msg) = ws.next().await {
        match msg? {
            Message::Text(t) => {
                let val: serde_json::Value = serde_json::from_str(&t)?;
                if val.get("id").is_some() {
                    info!("{feed_name}: server: {}", t);
                    continue;
                }
                let channel = val
                    .get("params")
                    .and_then(|p| p.get("channel"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let ev = RawEvent {
                    feed: feed_name,
                    stream: channel.clone(),
                    ts_recv_ns: Utc::now().timestamp_nanos_opt().unwrap_or(0),
                    payload: val,
                };
                if tx.send(ev).await.is_err() { break; }
                *counts.entry(channel).or_insert(0) += 1;
                if last_log.elapsed() >= Duration::from_secs(10) {
                    info!("{feed_name} counts(10s): {:?}", counts);
                    counts.clear();
                    last_log = std::time::Instant::now();
                }
            }
            Message::Ping(p) => ws.send(Message::Pong(p)).await?,
            Message::Close(c) => { warn!("{feed_name}: closed {:?}", c); break; }
            _ => {}
        }
    }
    Ok(())
}
