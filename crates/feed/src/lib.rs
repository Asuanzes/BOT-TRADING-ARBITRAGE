use btcbot_core::MarketSnapshot;
use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

const GAMMA: &str = "https://gamma-api.polymarket.com";
const CLOB: &str  = "https://clob.polymarket.com";
const BINANCE: &str = "https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT";
const POLL_SECS: u64 = 3;

pub fn spawn(tx: mpsc::Sender<MarketSnapshot>) {
    tokio::spawn(async move { feed_loop(tx).await });
}

struct WindowState {
    slug:        String,
    condition_id: String,
    token_up:    String,
    token_down:  String,
    window_start_ns: i64,
    window_end_ns:   i64,
    start_price: f64,   // BTC price at window open (beat/reference price)
}

async fn feed_loop(tx: mpsc::Sender<MarketSnapshot>) {
    info!("feed: starting btc-updown-5m (poll every {}s)", POLL_SECS);
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(5)).build() {
        Ok(c) => c,
        Err(e) => { error!("feed: {e}"); return; }
    };
    let mut tick = interval(Duration::from_secs(POLL_SECS));
    let mut state: Option<WindowState> = None;

    loop {
        tick.tick().await;

        let now_s = Utc::now().timestamp();
        let end_ts = (now_s / 300 + 1) * 300;
        let slug   = format!("btc-updown-5m-{end_ts}");

        // Fetch market when slug changes (new window)
        if state.as_ref().map(|s| &s.slug) != Some(&slug) {
            match fetch_window(&client, &slug).await {
                Some(s) => {
                    info!("feed: new window {} | Up={:.3} Down={:.3}",
                        slug, s.start_price, s.start_price);
                    state = Some(s);
                }
                None => {
                    warn!("feed: could not fetch {slug}");
                    state = None;
                    continue;
                }
            }
        }

        let s = match state.as_mut() { Some(s) => s, None => continue };

        let btc = match btc_price(&client).await {
            Some(p) => p,
            None => { warn!("feed: binance unavailable"); continue; }
        };
        if s.start_price == 0.0 { s.start_price = btc; }

        let (buy_up, buy_down) = tokio::join!(
            clob_buy(&client, &s.token_up),
            clob_buy(&client, &s.token_down),
        );

        let now_ns = Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let snap = MarketSnapshot {
            timestamp_ns:    now_ns,
            market_id:       s.condition_id.clone(),
            reference_price: btc,
            strike_price:    s.start_price,
            yes_price:       buy_up,
            no_price:        buy_down,
            window_start_ns: s.window_start_ns,
            window_end_ns:   s.window_end_ns,
        };

        info!("feed: btc={btc:.0} beat={:.0} diff={:+.0} up_buy={buy_up:.4} down_buy={buy_down:.4} | {}s left",
            s.start_price, btc - s.start_price,
            (s.window_end_ns - now_ns) / 1_000_000_000);

        if tx.send(snap).await.is_err() { return; }
    }
}

async fn fetch_window(client: &reqwest::Client, slug: &str) -> Option<WindowState> {
    let url = format!("{GAMMA}/markets?slug={slug}");
    let data: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;
    let markets = data.as_array().cloned()
        .or_else(|| data.get("markets")?.as_array().cloned())?;

    let m = markets.into_iter()
        .find(|x| x.get("question").and_then(|q| q.as_str())
            .map(|q| q.to_lowercase().contains("bitcoin")).unwrap_or(false))?;

    let condition_id = m.get("conditionId")?.as_str()?.to_string();
    let ids = m.get("clobTokenIds")?.as_array()?;
    let token_up   = ids.get(0)?.as_str()?.to_string();
    let token_down = ids.get(1)?.as_str()?.to_string();

    // startDate is window start; endDate is window end
    let start_ns = parse_ts(m.get("startDate").and_then(|d| d.as_str()).unwrap_or(""))?;
    let end_ns   = parse_ts(m.get("endDate").and_then(|d| d.as_str()).unwrap_or(""))?;

    // Fetch initial BTC price as beat price
    let start_price = btc_price(client).await.unwrap_or(0.0);

    Some(WindowState { slug: slug.to_string(), condition_id, token_up, token_down,
        window_start_ns: start_ns, window_end_ns: end_ns, start_price })
}

async fn btc_price(client: &reqwest::Client) -> Option<f64> {
    let v: serde_json::Value = client.get(BINANCE).send().await.ok()?.json().await.ok()?;
    v.get("price")?.as_str()?.parse().ok()
}

async fn clob_buy(client: &reqwest::Client, token_id: &str) -> f64 {
    let url = format!("{CLOB}/price?token_id={token_id}&side=buy");
    let v: serde_json::Value = match client.get(&url).send().await {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => return 0.5,
    };
    v.get("price").and_then(|p| p.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.5)
}

fn parse_ts(iso: &str) -> Option<i64> {
    if iso.is_empty() { return None; }
    let s = iso.replace('Z', "+00:00");
    chrono::DateTime::parse_from_rfc3339(&s).ok()?.timestamp_nanos_opt()
}
