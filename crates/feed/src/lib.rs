use btcbot_core::{MarketConfig, MarketSnapshot};
use chainlink::ChainlinkClient;
use chrono::Utc;
use std::collections::VecDeque;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

const GAMMA: &str = "https://gamma-api.polymarket.com";
const CLOB: &str  = "https://clob.polymarket.com";
const BINANCE: &str = "https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT";
const POLL_MS: u64 = 250;

// BTC_5M_UPDOWN window: the active Polymarket slot always spans exactly 300 s.
const WINDOW_NS: i64 = 300 * 1_000_000_000;

/// Spawns a polling task for the given market configuration.
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
    /// Tracks when the last 1-minute close was pushed.
    last_minute_ts_ns: i64,
    /// Rolling 1-minute BTC closes (up to 16, oldest first). Used by decision for RSI/momentum.
    minute_closes: VecDeque<f64>,
    /// Rolling 5-minute volume samples (up to 12, oldest first). Used for surge detection.
    volume_samples: VecDeque<f64>,
}

async fn feed_loop(tx: mpsc::Sender<MarketSnapshot>) {
    info!("feed: starting btc-updown-5m (poll every {}ms)", POLL_MS);

    let client = match reqwest::Client::builder().timeout(Duration::from_secs(5)).build() {
        Ok(c) => c,
        Err(e) => { error!("feed: {e}"); return; }
    };

    // Optional Chainlink oracle for authoritative BTC price.
    let chainlink = ChainlinkClient::from_env().ok();
    if chainlink.is_some() {
        info!("feed: Chainlink BTC/USD oracle activo");
    } else {
        info!("feed: Chainlink no configurado — usando Binance");
    }

    let mut tick = interval(Duration::from_millis(POLL_MS));
    let mut state: Option<WindowState> = None;

    loop {
        tick.tick().await;

        let now_s = Utc::now().timestamp();
        let window_start_ts = (now_s / 300) * 300;
        let slug = format!("btc-updown-5m-{window_start_ts}");

        // Fetch market when slug changes (new window)
        if state.as_ref().map(|s| &s.slug) != Some(&slug) {
            // Carry price and volume history across windows so RSI and volume-surge
            // have enough data points even on the first tick of a new window.
            let old_closes  = state.as_ref().map(|s| s.minute_closes.clone()).unwrap_or_default();
            let old_volumes = state.as_ref().map(|s| s.volume_samples.clone()).unwrap_or_default();
            match fetch_window(&client, &slug, old_volumes).await {
                Some(mut s) => {
                    s.minute_closes = old_closes;
                    info!("feed: new window {} | beat={:.0}", slug, s.start_price);
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

        // BTC price: Chainlink primary if available, Binance fallback.
        let btc = if let Some(ref cl) = chainlink {
            match tokio::time::timeout(Duration::from_millis(400), cl.get_btc_price_now()).await {
                Ok(Ok(p))  => p.benchmark_price,
                Ok(Err(_)) | Err(_) => match btc_price(&client).await {
                    Some(p) => p,
                    None => { warn!("feed: all price oracles unavailable"); continue; }
                },
            }
        } else {
            match btc_price(&client).await {
                Some(p) => p,
                None => { warn!("feed: binance unavailable"); continue; }
            }
        };
        if s.start_price == 0.0 { s.start_price = btc; }

        let (buy_up, buy_down) = tokio::join!(
            clob_buy(&client, &s.token_up),
            clob_buy(&client, &s.token_down),
        );

        let now_ns = Utc::now().timestamp_nanos_opt().unwrap_or(0);

        // Instantaneous momentum from the last sample.
        let momentum_usd_per_sec = if s.last_ref_price > 0.0 && s.last_ref_ts_ns > 0 {
            let dt_secs = (now_ns - s.last_ref_ts_ns) as f64 / 1e9;
            if dt_secs > 0.0 { (btc - s.last_ref_price) / dt_secs } else { 0.0 }
        } else {
            0.0
        };
        s.last_ref_price = btc;
        s.last_ref_ts_ns = now_ns;

        // 1-minute close tracking: push a new close every 60 s.
        if s.last_minute_ts_ns == 0 {
            s.last_minute_ts_ns = now_ns;
        }
        let secs_since_close = (now_ns - s.last_minute_ts_ns) as f64 / 1e9;
        let new_minute = secs_since_close >= 60.0;
        if new_minute {
            s.minute_closes.push_back(btc);
            if s.minute_closes.len() > 16 { s.minute_closes.pop_front(); }
            s.last_minute_ts_ns = now_ns;
        }

        // Spread: round-trip cost = (yes_ask + no_ask - 1.0).
        // Use whichever side has a quote; 0.0 only when BOTH are missing (CLOB down).
        let spread = if buy_up > 0.0 || buy_down > 0.0 {
            (buy_up + buy_down - 1.0).abs()
        } else {
            0.0  // both 0.0 → CLOB unreachable
        };

        let snap = MarketSnapshot {
            timestamp_ns:         now_ns,
            market_id:            s.condition_id.clone(),
            reference_price:      btc,
            strike_price:         s.start_price,
            yes_price:            buy_up,
            no_price:             buy_down,
            window_start_ns:      s.window_start_ns,
            window_end_ns:        s.window_end_ns,
            momentum_usd_per_sec,
            liquidity_yes:        0.0,
            liquidity_no:         0.0,
            volume_5m:            0.0,
            spread,
            token_id_yes:         s.token_up.clone(),
            token_id_no:          s.token_down.clone(),
            neg_risk:             false,
            oi_5m_pct:            0.0,
            price_history:        s.minute_closes.iter().copied().collect(),
            volume_history:       s.volume_samples.iter().copied().collect(),
        };

        info!("feed: btc={btc:.0} beat={:.0} diff={:+.0} mom={momentum_usd_per_sec:+.2}$/s up={buy_up:.4} dn={buy_down:.4} spread={spread:.4} hist={} vols={} | {}s left",
            s.start_price, btc - s.start_price,
            s.minute_closes.len(), s.volume_samples.len(),
            (s.window_end_ns - now_ns) / 1_000_000_000);

        if tx.send(snap).await.is_err() { return; }
    }
}

async fn fetch_window(
    client: &reqwest::Client,
    slug: &str,
    prior_volumes: VecDeque<f64>,
) -> Option<WindowState> {
    let url = format!("{GAMMA}/markets?slug={slug}");
    let data: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;
    let markets = data.as_array().cloned()
        .or_else(|| data.get("markets")?.as_array().cloned())?;

    let m = markets.into_iter()
        .find(|x| x.get("question").and_then(|q| q.as_str())
            .map(|q| q.to_lowercase().contains("bitcoin")).unwrap_or(false))?;

    let condition_id = m.get("conditionId")?.as_str()?.to_string();
    // clobTokenIds may be a JSON-encoded string or a real array
    let ids: Vec<String> = match m.get("clobTokenIds")? {
        serde_json::Value::Array(arr) => arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        serde_json::Value::String(s) => serde_json::from_str(s).ok()?,
        _ => return None,
    };
    let token_up   = ids.first()?.clone();
    let token_down = ids.get(1)?.clone();

    // window_start comes from the slug's trailing unix ts.
    let start_ts_secs: i64 = slug.rsplit_once('-')
        .and_then(|(_, ts)| ts.parse().ok())?;
    let start_ns = start_ts_secs * 1_000_000_000;
    let end_ns   = parse_ts(m.get("endDate").and_then(|d| d.as_str()).unwrap_or(""))?;
    if end_ns - start_ns != WINDOW_NS {
        warn!("feed: {slug} window is {}s, expected 300s — skipping",
            (end_ns - start_ns) / 1_000_000_000);
        return None;
    }

    // Beat price: Polymarket resolves via Chainlink Data Streams (BTC/USD index),
    // which aggregates multiple exchanges. We approximate it with the median of
    // Binance kline-open + Coinbase spot + Kraken spot, which tracks Chainlink
    // more closely than any single source alone.
    let (klines_raw, coinbase_p, kraken_p) = tokio::join!(
        klines_raw(client),
        btc_coinbase(client),
        btc_kraken(client),
    );
    let binance_open = kline_open(&klines_raw);
    let start_price  = median_price([binance_open, coinbase_p, kraken_p]);
    info!("feed: beat sources — binance={} coinbase={} kraken={} → median={:.2}",
        binance_open.map(|p| format!("{p:.2}")).unwrap_or_else(|| "?".into()),
        coinbase_p  .map(|p| format!("{p:.2}")).unwrap_or_else(|| "?".into()),
        kraken_p    .map(|p| format!("{p:.2}")).unwrap_or_else(|| "?".into()),
        start_price);

    // Seed volume history from the same klines response.
    let volume_samples = {
        let fresh = klines_to_volumes(&klines_raw);
        if !fresh.is_empty() { fresh } else { prior_volumes }
    };

    Some(WindowState {
        slug: slug.to_string(), condition_id, token_up, token_down,
        window_start_ns: start_ns, window_end_ns: end_ns, start_price,
        last_ref_price: 0.0, last_ref_ts_ns: 0,
        last_minute_ts_ns: 0,
        minute_closes: VecDeque::new(),
        volume_samples,
    })
}

/// Fetches the last 13 5-minute BTC/USDT klines from Binance (raw JSON).
/// Index 0..11 are completed candles; index 12 is the current (still-open) candle.
async fn klines_raw(client: &reqwest::Client) -> serde_json::Value {
    const URL: &str = "https://api.binance.com/api/v3/klines\
                       ?symbol=BTCUSDT&interval=5m&limit=13";
    match client.get(URL).send().await {
        Ok(r) => r.json::<serde_json::Value>().await.unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    }
}

/// Returns the open price of the current (still-open) 5-min kline — the price
/// at the exact 5-minute UTC boundary that Polymarket's oracle also samples.
fn kline_open(raw: &serde_json::Value) -> Option<f64> {
    let arr = raw.as_array()?;
    // Last element is the current candle; index 1 = open price.
    let last = arr.last()?;
    last.get(1)?.as_str()?.parse().ok()
}

/// Extracts completed-candle volumes (USDC = volume_btc × close) from raw klines.
fn klines_to_volumes(raw: &serde_json::Value) -> VecDeque<f64> {
    let arr = match raw.as_array() {
        Some(a) if a.len() > 1 => a,
        _ => return VecDeque::new(),
    };
    // Drop the last (still-open) candle.
    let completed = &arr[..arr.len() - 1];
    let mut out = VecDeque::with_capacity(12);
    for c in completed.iter().rev().take(12).collect::<Vec<_>>().into_iter().rev() {
        let close_price: f64 = c.get(4).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let volume_btc:  f64 = c.get(5).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0);
        if close_price > 0.0 {
            out.push_back(volume_btc * close_price);
        }
    }
    out
}

async fn btc_price(client: &reqwest::Client) -> Option<f64> {
    let v: serde_json::Value = client.get(BINANCE).send().await.ok()?.json().await.ok()?;
    v.get("price")?.as_str()?.parse().ok()
}

async fn btc_coinbase(client: &reqwest::Client) -> Option<f64> {
    let v: serde_json::Value = client
        .get("https://api.coinbase.com/v2/prices/BTC-USD/spot")
        .send().await.ok()?.json().await.ok()?;
    v.get("data")?.get("amount")?.as_str()?.parse().ok()
}

async fn btc_kraken(client: &reqwest::Client) -> Option<f64> {
    let v: serde_json::Value = client
        .get("https://api.kraken.com/0/public/Ticker?pair=XBTUSD")
        .send().await.ok()?.json().await.ok()?;
    let pair = v.get("result")?.as_object()?.values().next()?;
    pair.get("c")?.as_array()?.first()?.as_str()?.parse().ok()
}

/// Returns the median of up to 3 optional prices. Falls back to 0.0 if all are None.
fn median_price(prices: [Option<f64>; 3]) -> f64 {
    let mut vals: Vec<f64> = prices.into_iter().flatten().collect();
    if vals.is_empty() { return 0.0; }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    vals[vals.len() / 2]
}

async fn clob_buy(client: &reqwest::Client, token_id: &str) -> f64 {
    let url = format!("{CLOB}/price?token_id={token_id}&side=buy");
    // Return 0.0 on any error — signals "no data" to the decision engine (StaleQuotes)
    // and to the dashboard. 0.5 was ambiguous (looks like a valid mid-market price).
    let v: serde_json::Value = match client.get(&url).send().await {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => return 0.0,
    };
    v.get("price").and_then(|p| p.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0)
}

fn parse_ts(iso: &str) -> Option<i64> {
    if iso.is_empty() { return None; }
    let s = iso.replace('Z', "+00:00");
    chrono::DateTime::parse_from_rfc3339(&s).ok()?.timestamp_nanos_opt()
}
