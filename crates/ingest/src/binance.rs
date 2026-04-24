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
    base_ws: String,
    streams: Vec<String>,
    tx: mpsc::Sender<RawEvent>,
) {
    let mut handles = vec![];
    for stream in streams {
        let base = base_ws.clone();
        let tx2 = tx.clone();
        handles.push(tokio::spawn(async move {
            run_stream(feed_name, base, stream, tx2).await;
        }));
    }
    for h in handles {
        h.await.ok();
    }
}

async fn run_stream(
    feed_name: &'static str,
    base_ws: String,
    stream: String,
    tx: mpsc::Sender<RawEvent>,
) {
    let url = format!("{}/{}", base_ws.trim_end_matches('/'), stream);
    let mut backoff = 1u64;
    loop {
        match stream_once(feed_name, &url, &stream, &tx).await {
            Ok(_) => warn!("{feed_name}[{stream}]: closed"),
            Err(e) => error!("{feed_name}[{stream}]: error: {e}"),
        }
        info!("{feed_name}[{stream}]: reconnect in {}s", backoff);
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

async fn stream_once(
    feed_name: &'static str,
    url: &str,
    stream_name: &str,
    tx: &mpsc::Sender<RawEvent>,
) -> Result<()> {
    let (mut ws, resp) = connect_async(url).await?;
    info!("{feed_name}[{stream_name}]: connected http {}", resp.status());

    let mut count = 0u64;
    let mut last_log = std::time::Instant::now();
    let mut first_msg_logged = false;

    while let Some(msg) = ws.next().await {
        let m = msg?;
        if !first_msg_logged {
            info!("{feed_name}[{stream_name}]: FIRST frame type: {}",
                match &m {
                    Message::Text(_) => "Text",
                    Message::Binary(_) => "Binary",
                    Message::Ping(_) => "Ping",
                    Message::Pong(_) => "Pong",
                    Message::Close(_) => "Close",
                    Message::Frame(_) => "Frame",
                });
            first_msg_logged = true;
        }
        match m {
            Message::Text(t) => {
                let payload: serde_json::Value = serde_json::from_str(&t)?;
                let ev = RawEvent {
                    feed: feed_name,
                    stream: stream_name.to_string(),
                    ts_recv_ns: Utc::now().timestamp_nanos_opt().unwrap_or(0),
                    payload,
                };
                if tx.send(ev).await.is_err() { break; }
                count += 1;
                if last_log.elapsed() >= Duration::from_secs(10) {
                    info!("{feed_name}[{stream_name}]: {}/10s", count);
                    count = 0;
                    last_log = std::time::Instant::now();
                }
            }
            Message::Binary(b) => {
                warn!("{feed_name}[{stream_name}]: BINARY frame ({} bytes) ignored", b.len());
            }
            Message::Ping(p) => ws.send(Message::Pong(p)).await?,
            Message::Close(c) => {
                warn!("{feed_name}[{stream_name}]: server closed {:?}", c);
                break;
            }
            Message::Pong(_) => {},
            Message::Frame(_) => {},
        }
    }
    Ok(())
}
