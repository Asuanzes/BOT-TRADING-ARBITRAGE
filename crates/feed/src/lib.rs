use btcbot_core::{MarketConfig, MarketSnapshot};
use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

const GAMMA: &str = "https://gamma-api.polymarket.com";
const CLOB: &str  = "https://clob.polymarket.com";
const BINANCE: &str = "https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT";
const POLL_SECS: u64 = 3;

// BTC_5M_UPDOWN window: the active Polymarket slot always spans exactly 300 s.
// window_start_ns = market open (UTC, unix ns); window_end_ns = resolution time.
// The market resolves YES if BTC at window_end > strike_price (BTC at window_start), NO otherwise.
// To add other market types, spawn a separate feed task with its own fetch_window variant.
const WINDOW_NS: i64 = 300 * 1_000_000_000;

/// Spawns a polling task for the given market configuration.
/// Here is where each additional market symbol gets its own feed loop.
/// To add ETH_5M_UPDOWN (or any other market):
///   1. Add a match arm for its id below.
///   2. Write a `feed_loop_eth` (or generic parameterized version) that uses
///      the correct Polymarket slug prefix and price-source URL from `cfg.symbol`.
pub fn spawn_for_market(cfg: &MarketConfig, tx: mpsc::Sender<MarketSnapshot>) {
    match cfg.id.as_str() {
        btcbot_core::BTC_5M_UPDOWN => {
            tokio::spawn(async move { feed_loop(tx).await });
        }
        other => warn!("feed: no implementation for market '{other}' — skipping"),
    }
}

struct WindowState {
    slug:        String,
    condition_id: String,
    token_up:    String,
    token_down:  String,
    window_start_ns: i64,
    window_end_ns:   i64,
    start_price: f64,   // BTC price at window open (beat/reference price)
    /// Previous underlying sample for instantaneous momentum calc. 0.0 = no prior sample yet.
    last_ref_price:  f64,
    last_ref_ts_ns:  i64,
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

        // Slug timestamp is the window START. Ask for the currently active window
        // (floor to 5-min boundary), not the next one — Polymarket only publishes
        // markets close to their start time, and the active window is the one we
        // can trade right now.
        let now_s = Utc::now().timestamp();
        let window_start_ts = (now_s / 300) * 300;
        let slug = format!("btc-updown-5m-{window_start_ts}");

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

        // Instantaneous momentum from the last sample. Zero for the first tick
        // of a window (or whenever the previous sample was skipped).
        let momentum_usd_per_sec = if s.last_ref_price > 0.0 && s.last_ref_ts_ns > 0 {
            let dt_secs = (now_ns - s.last_ref_ts_ns) as f64 / 1e9;
            if dt_secs > 0.0 { (btc - s.last_ref_price) / dt_secs } else { 0.0 }
        } else {
            0.0
        };
        s.last_ref_price = btc;
        s.last_ref_ts_ns = now_ns;

        // market_id = Polymarket condition ID (needed for order placement).
        // Logical name for this feed is BTC_5M_UPDOWN (btcbot_core::BTC_5M_UPDOWN).
        // strike_price = BTC price at window_start; reference_price = current BTC spot.
        let snap = MarketSnapshot {
            timestamp_ns:    now_ns,
            market_id:       s.condition_id.clone(),
            reference_price: btc,
            strike_price:    s.start_price,
            yes_price:       buy_up,
            no_price:        buy_down,
            window_start_ns: s.window_start_ns,
            window_end_ns:   s.window_end_ns,
            momentum_usd_per_sec,
        };

        info!("feed: btc={btc:.0} beat={:.0} diff={:+.0} mom={momentum_usd_per_sec:+.2}$/s up_buy={buy_up:.4} down_buy={buy_down:.4} | {}s left",
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

    // window_start comes from the slug's trailing unix ts — NOT from Polymarket's
    // `startDate` field, which is the market CREATION time (often hours or a day
    // before the trading window). `endDate` is the resolution time (window end).
    let start_ts_secs: i64 = slug.rsplit_once('-')
        .and_then(|(_, ts)| ts.parse().ok())?;
    let start_ns = start_ts_secs * 1_000_000_000;
    let end_ns   = parse_ts(m.get("endDate").and_then(|d| d.as_str()).unwrap_or(""))?;
    if end_ns - start_ns != WINDOW_NS {
        warn!("feed: {slug} window is {}s, expected 300s — skipping",
            (end_ns - start_ns) / 1_000_000_000);
        return None;
    }

    // Fetch initial BTC price as beat price
    let start_price = btc_price(client).await.unwrap_or(0.0);

    Some(WindowState {
        slug: slug.to_string(), condition_id, token_up, token_down,
        window_start_ns: start_ns, window_end_ns: end_ns, start_price,
        last_ref_price: 0.0, last_ref_ts_ns: 0,
    })
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
