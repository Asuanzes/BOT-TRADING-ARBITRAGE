use crate::types::RawEvent;
use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

const GAMMA: &str = "https://gamma-api.polymarket.com";
const CLOB: &str = "https://clob.polymarket.com";

pub fn spawn(tx: mpsc::Sender<RawEvent>) {
    tokio::spawn(async move {
        polymarket_loop(tx).await;
    });
}

async fn polymarket_loop(tx: mpsc::Sender<RawEvent>) {
    info!("polymarket: starting reader (60s)");
    let client = reqwest::Client::new();
    let mut tick = interval(Duration::from_secs(60));

    loop {
        tick.tick().await;
        let url = format!("{GAMMA}/markets?closed=false&tag_slug=btc&limit=50");
        match client.get(&url).send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(data) => {
                    let markets = match data.as_array() {
                        Some(a) => a.clone(),
                        None => match data.get("markets").and_then(|m| m.as_array()) {
                            Some(a) => a.clone(),
                            None => { warn!("polymarket: unexpected format"); continue; }
                        },
                    };
                    let mut found = 0;
                    for market in &markets {
                        let question = market.get("question").and_then(|q| q.as_str()).unwrap_or("");
                        if !question.to_lowercase().contains("bitcoin") && !question.to_lowercase().contains("btc") { continue; }
                        let end_date = market.get("endDateIso").and_then(|d| d.as_str()).unwrap_or("");
                        let mins = minutes_to_expiry(end_date);
                        if mins < 0 || mins > 15 { continue; }
                        found += 1;
                        let condition_id = market.get("conditionId").and_then(|c| c.as_str()).unwrap_or("");
                        let yes_price = get_price(&client, market, "YES").await;
                        let no_price = get_price(&client, market, "NO").await;
                        let payload = serde_json::json!({
                            "condition_id": condition_id,
                            "question": question,
                            "end_date": end_date,
                            "mins_to_expiry": mins,
                            "yes_price": yes_price,
                            "no_price": no_price,
                            "volume": market.get("volume"),
                            "liquidity": market.get("liquidity"),
                        });
                        info!("polymarket: {} | yes={:.3} no={:.3} | {}min left", question, yes_price, no_price, mins);
                        let ev = RawEvent {
                            feed: "polymarket",
                            stream: "btc_5min".to_string(),
                            ts_recv_ns: Utc::now().timestamp_nanos_opt().unwrap_or(0),
                            payload,
                        };
                        if tx.send(ev).await.is_err() { error!("polymarket: channel closed"); return; }
                    }
                    if found == 0 { info!("polymarket: no active BTC 5min markets"); }
                }
                Err(e) => error!("polymarket: parse error: {e}"),
            },
            Err(e) => error!("polymarket: request error: {e}"),
        }
    }
}

async fn get_price(client: &reqwest::Client, market: &serde_json::Value, side: &str) -> f64 {
    let tokens = match market.get("tokens").and_then(|t| t.as_array()) {
        Some(t) => t.clone(),
        None => return 0.5,
    };
    let token = tokens.iter().find(|t| t.get("outcome").and_then(|o| o.as_str()).unwrap_or("") == side);
    let token_id = match token.and_then(|t| t.get("token_id")).and_then(|id| id.as_str()) {
        Some(id) => id.to_string(),
        None => return 0.5,
    };
    let url = format!("{CLOB}/midpoint?token_id={token_id}");
    match client.get(&url).send().await {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(data) => data.get("mid").and_then(|m| m.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.5),
            Err(_) => 0.5,
        },
        Err(_) => 0.5,
    }
}

fn minutes_to_expiry(end_date_iso: &str) -> i64 {
    if end_date_iso.is_empty() { return -1; }
    match chrono::DateTime::parse_from_rfc3339(end_date_iso) {
        Ok(end) => (end.with_timezone(&Utc) - Utc::now()).num_minutes(),
        Err(_) => -1,
    }
}
