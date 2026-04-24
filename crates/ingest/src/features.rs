use crate::types::RawEvent;
use chrono::Utc;
use std::collections::VecDeque;
use tokio::sync::mpsc;
use tracing::info;

#[derive(Clone, Debug)]
struct Trade {
    price: f64,
    size: f64,
    is_buy: bool,
}

struct FeatureState {
    start_time: std::time::Instant,
    trades: Vec<Trade>,
    bid_levels: Vec<(f64, f64)>,
    ask_levels: Vec<(f64, f64)>,
    last_price: f64,
    funding_rate: f64,
    tick_count: u64,
}

pub fn spawn(rx: mpsc::UnboundedReceiver<RawEvent>) {
    tokio::spawn(async move { features_loop(rx).await; });
}

async fn features_loop(mut rx: mpsc::UnboundedReceiver<RawEvent>) {
    info!("features: starting 5min feature calculator");
    let mut state = FeatureState {
        start_time: std::time::Instant::now(),
        trades: Vec::new(),
        bid_levels: Vec::new(),
        ask_levels: Vec::new(),
        last_price: 0.0,
        funding_rate: 0.0,
        tick_count: 0,
    };
    let mut candles_1m: VecDeque<f64> = VecDeque::new();
    let mut last_minute_sec = 0u64;
    while let Some(ev) = rx.recv().await {
        state.tick_count += 1;
        match ev.stream.as_str() {
            s if s.contains("depth") || s.contains("orderbook") => {
                parse_orderbook(&ev.payload, &mut state.bid_levels, &mut state.ask_levels);
            }
            s if s.contains("aggTrade") || s.contains("publicTrade") => {
                if let Some(trade) = parse_trade(&ev.payload) {
                    state.last_price = trade.price;
                    state.trades.push(trade);
                }
            }
            s if s.contains("ticker") || s.contains("tickers") => {
                if let Some((mark, funding)) = parse_ticker(&ev.payload) {
                    state.last_price = mark;
                    if funding != 0.0 { if funding != 0.0 { state.funding_rate = funding; } }
                }
            }
            _ => {}
        }
        let now_sec = Utc::now().timestamp() as u64;
        if now_sec > last_minute_sec + 60 {
            if state.last_price > 0.0 {
                candles_1m.push_back(state.last_price);
                if candles_1m.len() > 5 { candles_1m.pop_front(); }
            }
            last_minute_sec = now_sec;
        }
        if state.start_time.elapsed().as_secs() >= 300 {
            let hour = Utc::now().format("%H").to_string();
            let ob_imbal = calc_ob_imbalance(&state.bid_levels, &state.ask_levels);
            let cvd = calc_cvd(&state.trades);
            let vol_1m = calc_volatility(&candles_1m);
            let vol_5m = calc_volatility_5m(&state.trades);
            let spread = calc_spread(&state.bid_levels, &state.ask_levels);
            let momentum = calc_momentum(&state.trades);
            info!("5min_features | hour={} | ob_imbal={:+.4} | cvd={:+.2} | vol_1m={:.6} | vol_5m={:.6} | spread={:.8} | momentum={:+.2} | funding={:+.6} | trades={} | ticks={}", hour, ob_imbal, cvd, vol_1m, vol_5m, spread, momentum, state.funding_rate, state.trades.len(), state.tick_count);
            state = FeatureState {
                start_time: std::time::Instant::now(),
                trades: Vec::new(),
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                last_price: state.last_price,
                funding_rate: state.funding_rate,
                tick_count: 0,
            };
        }
    }
    info!("features: closed");
}

fn parse_orderbook(payload: &serde_json::Value, bids: &mut Vec<(f64, f64)>, asks: &mut Vec<(f64, f64)>) {
    let p = payload.get("data").unwrap_or(payload);
    if let Some(arr) = p.get("bids").or(p.get("b")).and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            bids.clear();
            for item in arr.iter().take(5) {
                if let Some(a) = item.as_array() {
                    if let (Some(ps), Some(vs)) = (a.get(0).and_then(|x| x.as_str()), a.get(1).and_then(|x| x.as_str())) {
                        if let (Ok(p), Ok(v)) = (ps.parse::<f64>(), vs.parse::<f64>()) { bids.push((p, v)); }
                    }
                }
            }
        }
    }
    if let Some(arr) = p.get("asks").or(p.get("a")).and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            asks.clear();
            for item in arr.iter().take(5) {
                if let Some(a) = item.as_array() {
                    if let (Some(ps), Some(vs)) = (a.get(0).and_then(|x| x.as_str()), a.get(1).and_then(|x| x.as_str())) {
                        if let (Ok(p), Ok(v)) = (ps.parse::<f64>(), vs.parse::<f64>()) { asks.push((p, v)); }
                    }
                }
            }
        }
    }
}

fn parse_trade(payload: &serde_json::Value) -> Option<Trade> {
    let price = payload.get("p").or(payload.get("price")).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok())?;
    let size = payload.get("q").or(payload.get("size")).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok())?;
    let is_buy = payload.get("m").or(payload.get("side")).and_then(|v| match v {
        serde_json::Value::Bool(b) => Some(!b),
        serde_json::Value::String(s) => Some(s.to_lowercase() == "buy"),
        _ => None,
    }).unwrap_or(true);
    Some(Trade { price, size, is_buy })
}

fn parse_ticker(payload: &serde_json::Value) -> Option<(f64, f64)> {
    let p = payload.pointer("/params/data")
        .or_else(|| payload.get("data"))
        .unwrap_or(payload);
    let get_num = |k: &str| -> Option<f64> {
        let v = p.get(k)?;
        v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
    };
    let mark = get_num("mark_price")
        .or_else(|| get_num("markPrice"))
        .or_else(|| get_num("last_price"))
        .or_else(|| get_num("lastPrice"))?;
    let funding = get_num("current_funding")
        .or_else(|| get_num("funding_rate"))
        .or_else(|| get_num("fundingRate"))
        .or_else(|| get_num("funding_8h"))
        .unwrap_or(0.0);
    Some((mark, funding))
}

fn calc_ob_imbalance(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> f64 {
    let bv: f64 = bids.iter().map(|(_, v)| v).sum();
    let av: f64 = asks.iter().map(|(_, v)| v).sum();
    let t = bv + av;
    if t > 0.0 { (bv - av) / t } else { 0.0 }
}

fn calc_cvd(trades: &[Trade]) -> f64 {
    trades.iter().map(|t| if t.is_buy { t.size } else { -t.size }).sum()
}

fn calc_volatility(candles: &VecDeque<f64>) -> f64 {
    if candles.len() < 2 { return 0.0; }
    let mut r = Vec::new();
    for i in 1..candles.len() { r.push((candles[i] / candles[i - 1]).ln()); }
    std_dev(&r)
}

fn calc_volatility_5m(trades: &[Trade]) -> f64 {
    if trades.len() < 2 { return 0.0; }
    let fp = trades[0].price;
    let mut r = Vec::new();
    for t in &trades[1..] { r.push((t.price / fp).ln()); }
    std_dev(&r)
}

fn calc_spread(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> f64 {
    if bids.is_empty() || asks.is_empty() { return 0.0; }
    let mid = (bids[0].0 + asks[0].0) / 2.0;
    if mid > 0.0 { (asks[0].0 - bids[0].0) / mid } else { 0.0 }
}

fn calc_momentum(trades: &[Trade]) -> f64 {
    if trades.is_empty() { return 0.0; }
    let n = trades.len().min(5);
    let s: i32 = trades[trades.len() - n..].iter().map(|t| if t.is_buy { 1 } else { -1 }).sum();
    s as f64 / n as f64
}

fn std_dev(v: &[f64]) -> f64 {
    if v.is_empty() { return 0.0; }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64;
    var.sqrt()
}
