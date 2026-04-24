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
    topics: Vec<String>,
    tx: mpsc::Sender<RawEvent>,
) {
    let mut backoff = 1u64;
    loop {
        match stream_once(feed_name, &url, &topics, &tx).await {
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
    topics: &[String],
    tx: &mpsc::Sender<RawEvent>,
) -> Result<()> {
    let (mut ws, resp) = connect_async(url).await?;
    info!("{feed_name}: connected http {}", resp.status());

    let sub = serde_json::json!({ "op": "subscribe", "args": topics });
    ws.send(Message::Text(sub.to_string().into())).await?;
    info!("{feed_name}: SUBSCRIBE -> {:?}", topics);

    let mut counts: std::collections::HashMap<String, u64> = Default::default();
    let mut last_log = std::time::Instant::now();
    let mut ping_interval = tokio::time::interval(Duration::from_secs(20));
    ping_interval.tick().await;

    loop {
        tokio::select! {
            msg = ws.next() => {
                let Some(msg) = msg else { break; };
                match msg? {
                    Message::Text(t) => {
                        let val: serde_json::Value = serde_json::from_str(&t)?;
                        let op = val.get("op").and_then(|v| v.as_str()).unwrap_or("");
                        if op == "pong" || op == "subscribe" || val.get("success").is_some() {
                            info!("{feed_name}: server: {}", t);
                            continue;
                        }
                        let topic = val
                            .get("topic")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let ev = RawEvent {
                            feed: feed_name,
                            stream: topic.clone(),
                            ts_recv_ns: Utc::now().timestamp_nanos_opt().unwrap_or(0),
                            payload: val,
                        };
                        if tx.send(ev).await.is_err() { break; }
                        *counts.entry(topic).or_insert(0) += 1;
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
            _ = ping_interval.tick() => {
                let ping = serde_json::json!({"op":"ping"});
                ws.send(Message::Text(ping.to_string().into())).await?;
            }
        }
    }
    Ok(())
}
