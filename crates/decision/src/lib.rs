use btcbot_core::{DecisionConfig, Direction, MarketConfig, MarketSnapshot, Signal};

/// Builds a DecisionConfig from any MarketConfig entry.
pub fn decision_config_for(cfg: &MarketConfig) -> DecisionConfig {
    DecisionConfig {
        min_price_diff_usd:          cfg.min_price_diff_usd,
        entry_delay_secs:            cfg.entry_delay_secs,
        min_remaining_secs:          cfg.min_remaining_secs,
        max_remaining_secs:          cfg.max_remaining_secs,
        max_entry_token_price_yes:   cfg.max_entry_token_price_yes,
        max_entry_token_price_no:    cfg.max_entry_token_price_no,
        min_entry_token_price:       cfg.min_entry_token_price,
        momentum_reject_usd_per_sec: cfg.momentum_reject_usd_per_sec,
        min_volume_5m:               cfg.min_volume_5m,
        min_liq_multiplier:          cfg.min_liq_multiplier,
        max_spread:                  cfg.max_spread,
        max_quote_skew:              cfg.max_quote_skew,
        min_entry_edge:              cfg.min_entry_edge,
        vol_usd_per_sqrts:           cfg.vol_usd_per_sqrts,
        strong_momentum_usd_per_sec: cfg.strong_momentum_usd_per_sec,
        min_price_diff_usd_strong:   cfg.min_price_diff_usd_strong,
        oi_contrarian_pct:           cfg.oi_contrarian_pct,
        fade_extreme_diff_usd:       cfg.fade_extreme_diff_usd,
        dead_zone_start_secs:        cfg.dead_zone_start_secs,
        dead_zone_end_secs:          cfg.dead_zone_end_secs,
        entry_size_usdc:             150.0,
        operation_mode:              cfg.operation_mode,
    }
}

/// Per-signal breakdown of the weighted score.
#[derive(Debug, Clone)]
pub struct ScoreInfo {
    pub window_delta: f64,   // ±5 or ±7  — BTC % vs strike
    pub momentum:     f64,   // ±2        — EMA(3)/EMA(10) crossover (Makarewicz)
    pub rsi:          f64,   // ±1 or ±2  — RSI 14-period
    pub volume:       f64,   // ±1        — volume surge
    pub arb_edge:     f64,   // ±1        — market inefficiency YES+NO vs 1.0 (PolyScripts)
    pub total:        f64,
    pub confidence:   f64,   // |total| / 7.0 capped at 1.0
}

/// Computes per-signal score breakdown without running the full filter chain.
/// Safe to call every tick; does not depend on DecisionConfig.
pub fn score_info(snapshot: &MarketSnapshot) -> ScoreInfo {
    let mut window_delta = 0.0f64;
    let mut momentum     = 0.0f64;
    let mut rsi_score    = 0.0f64;
    let mut volume_score = 0.0f64;
    let mut arb_edge     = 0.0f64;

    // 1. Window Delta
    if snapshot.strike_price > 0.0 {
        let window_pct = (snapshot.reference_price - snapshot.strike_price)
            / snapshot.strike_price * 100.0;
        if      window_pct >  0.10 { window_delta =  7.0; }
        else if window_pct >  0.02 { window_delta =  5.0; }
        else if window_pct < -0.10 { window_delta = -7.0; }
        else if window_pct < -0.02 { window_delta = -5.0; }
    }

    // 2. Momentum — EMA(3)/EMA(10) crossover (Makarewicz); micro-momentum fallback
    let n = snapshot.price_history.len();
    if n >= 10 {
        let ema3  = compute_ema(&snapshot.price_history, 3);
        let ema10 = compute_ema(&snapshot.price_history, 10);
        if      ema3 > ema10 * 1.0001 { momentum =  2.0; }
        else if ema3 < ema10 * 0.9999 { momentum = -2.0; }
    } else if n >= 2 {
        let c1 = snapshot.price_history[n - 1];
        let c0 = snapshot.price_history[n - 2];
        if n >= 3 {
            let cp = snapshot.price_history[n - 3];
            if      c1 > c0 && c0 > cp { momentum =  2.0; }
            else if c1 < c0 && c0 < cp { momentum = -2.0; }
        } else if c1 > c0 { momentum =  2.0; }
          else if c1 < c0 { momentum = -2.0; }
    }

    // 3. RSI 14-period
    let score_mid = window_delta + momentum;
    if n >= 14 {
        let rsi = compute_rsi(&snapshot.price_history);
        if      rsi > 75.0      { rsi_score = -2.0; }
        else if rsi < 25.0      { rsi_score =  2.0; }
        else if score_mid > 0.0 { rsi_score =  1.0; }
        else if score_mid < 0.0 { rsi_score = -1.0; }
    }

    // 4. Volume Surge
    let score_prev = score_mid + rsi_score;
    if snapshot.volume_history.len() >= 3 {
        let avg: f64 = snapshot.volume_history.iter().sum::<f64>()
            / snapshot.volume_history.len() as f64;
        if let Some(&recent) = snapshot.volume_history.last() {
            if avg > 0.0 && recent > 1.5 * avg {
                if      score_prev > 0.0 { volume_score =  1.0; }
                else if score_prev < 0.0 { volume_score = -1.0; }
            }
        }
    }

    // 5. Arbitrage Edge — PolyScripts: market inefficiency YES+NO vs 1.0.
    // A gap > 0.03 (3c) means one side is underpriced → confirms direction.
    // A negative gap (sum > 1.03) means both sides are overpriced → fades direction.
    let score_before_arb = score_prev + volume_score;
    if snapshot.yes_price > 0.0 && snapshot.no_price > 0.0 {
        let gap = 1.0 - (snapshot.yes_price + snapshot.no_price);
        if gap > 0.03 {
            if      score_before_arb > 0.0 { arb_edge =  1.0; }
            else if score_before_arb < 0.0 { arb_edge = -1.0; }
        } else if gap < -0.03 {
            if      score_before_arb > 0.0 { arb_edge = -1.0; }
            else if score_before_arb < 0.0 { arb_edge =  1.0; }
        }
    }

    let total      = score_before_arb + arb_edge;
    let confidence = (total.abs() / 7.0).min(1.0);
    ScoreInfo { window_delta, momentum, rsi: rsi_score, volume: volume_score, arb_edge, total, confidence }
}

/// Black-Scholes-derived fair value for the UP side (P(BTC > strike) at expiry).
/// Returns None when inputs are unavailable (no Chainlink feed or zero remaining time).
pub fn fair_value_up(snapshot: &MarketSnapshot, vol_usd_per_sqrts: f64) -> Option<f64> {
    let remaining = snapshot.remaining_secs();
    if vol_usd_per_sqrts <= 0.0 || remaining <= 0.0 || snapshot.strike_price <= 0.0 {
        return None;
    }
    let diff      = snapshot.reference_price - snapshot.strike_price;
    let vol_total = vol_usd_per_sqrts * remaining.sqrt();
    Some(normal_cdf(diff / vol_total))
}

/// Why an entry was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    TooMuchTimeLeft,
    NearWindowEnd,
    SpreadTooWide,
    StaleQuotes,
    InsufficientLiquidity,
    TokenTooExpensive,
    TokenTooWeak,
    /// Score-based confidence is below the threshold for the configured operation mode.
    LowConfidence,
    /// Score is zero — signals are contradictory or absent.
    ScoreNeutral,
    /// Window has moved > 5% from open price — extreme volatility, skip entry.
    ExtremeVolatility,
    /// Token price is above its delta-based fair value — no edge, skip.
    NoEdge,
}

impl RejectReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RejectReason::TooMuchTimeLeft       => "too_much_time_left",
            RejectReason::NearWindowEnd         => "near_window_end",
            RejectReason::SpreadTooWide         => "spread_too_wide",
            RejectReason::StaleQuotes           => "stale_quotes",
            RejectReason::InsufficientLiquidity => "insufficient_liquidity",
            RejectReason::TokenTooExpensive     => "token_too_expensive",
            RejectReason::TokenTooWeak          => "token_too_weak",
            RejectReason::LowConfidence         => "low_confidence",
            RejectReason::ScoreNeutral          => "score_neutral",
            RejectReason::ExtremeVolatility     => "extreme_volatility",
            RejectReason::NoEdge                => "no_edge",
        }
    }
}

/// Result of evaluating a market snapshot.
#[derive(Debug)]
pub enum Evaluation {
    Enter(Signal),
    Reject(RejectReason),
}

/// Evaluates a snapshot against the given thresholds using a weighted scoring approach.
pub fn evaluate(snapshot: &MarketSnapshot, config: &DecisionConfig) -> Evaluation {
    // ── Timing gate: snipe T-30s to T-10s ─────────────────────────────────
    let remaining = snapshot.remaining_secs();
    if config.max_remaining_secs > 0.0 && remaining > config.max_remaining_secs {
        return Evaluation::Reject(RejectReason::TooMuchTimeLeft);
    }
    if remaining < config.min_remaining_secs {
        return Evaluation::Reject(RejectReason::NearWindowEnd);
    }

    // ── Safety guards (keep; degrade gracefully when data absent) ──────────
    if config.max_spread > 0.0 && snapshot.spread > 0.0 && snapshot.spread > config.max_spread {
        return Evaluation::Reject(RejectReason::SpreadTooWide);
    }
    if config.max_quote_skew > 0.0
        && snapshot.yes_price > 0.0 && snapshot.no_price > 0.0
        && (snapshot.yes_price + snapshot.no_price - 1.0).abs() > config.max_quote_skew
    {
        return Evaluation::Reject(RejectReason::StaleQuotes);
    }

    // ── Extreme volatility stop ────────────────────────────────────────────
    // Si la ventana ha movido más del 5%, el mercado está en modo "todo o nada"
    // y el token ya habrá priceado el movimiento — sin edge real, saltar.
    if snapshot.strike_price > 0.0 {
        let window_pct_abs = ((snapshot.reference_price - snapshot.strike_price)
            / snapshot.strike_price * 100.0).abs();
        if window_pct_abs > 5.0 {
            return Evaluation::Reject(RejectReason::ExtremeVolatility);
        }
    }

    // ── Weighted scoring ───────────────────────────────────────────────────
    let mut score: f64 = 0.0;

    // 1. Window Delta (weight 5-7)
    if snapshot.strike_price > 0.0 {
        let window_pct = (snapshot.reference_price - snapshot.strike_price)
            / snapshot.strike_price * 100.0;
        if window_pct > 0.10 {
            score += 7.0;
        } else if window_pct > 0.02 {
            score += 5.0;
        } else if window_pct < -0.10 {
            score -= 7.0;
        } else if window_pct < -0.02 {
            score -= 5.0;
        }
    }

    // 2. Momentum EMA(3)/EMA(10) crossover — Makarewicz; micro-momentum fallback when < 10 closes
    {
        let n = snapshot.price_history.len();
        if n >= 10 {
            let ema3  = compute_ema(&snapshot.price_history, 3);
            let ema10 = compute_ema(&snapshot.price_history, 10);
            if      ema3 > ema10 * 1.0001 { score += 2.0; }
            else if ema3 < ema10 * 0.9999 { score -= 2.0; }
        } else if n >= 2 {
            let c1 = snapshot.price_history[n - 1];
            let c0 = snapshot.price_history[n - 2];
            if n >= 3 {
                let cp = snapshot.price_history[n - 3];
                if      c1 > c0 && c0 > cp { score += 2.0; }
                else if c1 < c0 && c0 < cp { score -= 2.0; }
            } else if c1 > c0 { score += 2.0; }
              else if c1 < c0 { score -= 2.0; }
        }
    }

    // 3. RSI 14-period (weight 1-2)
    if snapshot.price_history.len() >= 14 {
        let rsi = compute_rsi(&snapshot.price_history);
        if rsi > 75.0 {
            score -= 2.0; // overbought → fade Up
        } else if rsi < 25.0 {
            score += 2.0; // oversold → fade Down
        } else {
            // neutral RSI: weight 1 in direction of score so far
            if score > 0.0 {
                score += 1.0;
            } else if score < 0.0 {
                score -= 1.0;
            }
        }
    }

    // 4. Volume Surge (weight 1): recent volume > 1.5× rolling average
    if snapshot.volume_history.len() >= 3 {
        let avg: f64 = snapshot.volume_history.iter().sum::<f64>()
            / snapshot.volume_history.len() as f64;
        if let Some(&recent) = snapshot.volume_history.last() {
            if avg > 0.0 && recent > 1.5 * avg {
                if      score > 0.0 { score += 1.0; }
                else if score < 0.0 { score -= 1.0; }
            }
        }
    }

    // 5. Arbitrage Edge (weight 1) — PolyScripts: YES+NO mispricing vs 1.0.
    // gap > 0.03 → market underpriced → extra edge in our direction.
    // gap < −0.03 → market overpriced → fade.
    if snapshot.yes_price > 0.0 && snapshot.no_price > 0.0 {
        let gap = 1.0 - (snapshot.yes_price + snapshot.no_price);
        if gap > 0.03 {
            if      score > 0.0 { score += 1.0; }
            else if score < 0.0 { score -= 1.0; }
        } else if gap < -0.03 {
            if      score > 0.0 { score -= 1.0; }
            else if score < 0.0 { score += 1.0; }
        }
    }

    // ── Direction and confidence ───────────────────────────────────────────
    if score == 0.0 {
        return Evaluation::Reject(RejectReason::ScoreNeutral);
    }
    let confidence = (score.abs() / 7.0).min(1.0);
    let direction = if score > 0.0 { Direction::Up } else { Direction::Down };

    if confidence < config.operation_mode.min_confidence() {
        return Evaluation::Reject(RejectReason::LowConfidence);
    }

    // ── Fair-price check (pricing realista por delta) ─────────────────────
    // fair_value = Φ(signed_diff / (vol × √remaining))
    // vol_usd_per_sqrts calibrated to ~25 USD/√s for BTC 5-min windows.
    // Skip if token is priced ABOVE fair value — we'd be overpaying.
    // Skips when vol_usd_per_sqrts == 0 (disabled) or remaining ≤ 0.
    if config.vol_usd_per_sqrts > 0.0 && remaining > 0.0 && snapshot.strike_price > 0.0 {
        let diff = snapshot.reference_price - snapshot.strike_price;
        let signed_diff = match direction { Direction::Up => diff, Direction::Down => -diff };
        let vol_total  = config.vol_usd_per_sqrts * remaining.sqrt();
        let fair_value = normal_cdf(signed_diff / vol_total);
        let quoted = match direction { Direction::Up => snapshot.yes_price, Direction::Down => snapshot.no_price };
        // Only reject when the token is clearly overpriced vs fair value (> 0.08 above).
        // Loose bound so we don't kill valid entries on noisy quotes.
        if quoted > 0.0 && quoted > fair_value + 0.08 {
            return Evaluation::Reject(RejectReason::NoEdge);
        }
    }

    // ── Liquidity guard ────────────────────────────────────────────────────
    if config.min_liq_multiplier > 0.0 {
        let target_liq = match direction {
            Direction::Up   => snapshot.liquidity_yes,
            Direction::Down => snapshot.liquidity_no,
        };
        let min_liq = config.min_liq_multiplier * config.entry_size_usdc;
        if target_liq > 0.0 && target_liq < min_liq {
            return Evaluation::Reject(RejectReason::InsufficientLiquidity);
        }
    }

    let (token_price, max_token_price) = match direction {
        Direction::Up   => (snapshot.yes_price, config.max_entry_token_price_yes),
        Direction::Down => (snapshot.no_price,  config.max_entry_token_price_no),
    };

    if token_price <= 0.0 {
        return Evaluation::Reject(RejectReason::StaleQuotes);
    }
    if token_price > max_token_price {
        return Evaluation::Reject(RejectReason::TokenTooExpensive);
    }
    if config.min_entry_token_price > 0.0 && token_price < config.min_entry_token_price {
        return Evaluation::Reject(RejectReason::TokenTooWeak);
    }

    Evaluation::Enter(Signal {
        direction,
        side: direction.to_side(),
        token_price,
        confidence,
        entry_at_ns: snapshot.timestamp_ns,
    })
}

/// EMA over the last `period` values (Makarewicz).  Uses all available prices
/// as the warm-up series so the first output is not cold-started at prices[0].
fn compute_ema(prices: &[f64], period: usize) -> f64 {
    if prices.is_empty() { return 0.0; }
    let alpha = 2.0 / (period as f64 + 1.0);
    let mut ema = prices[0];
    for &p in &prices[1..] {
        ema = alpha * p + (1.0 - alpha) * ema;
    }
    ema
}

/// Standard normal CDF (Abramowitz & Stegun 26.2.17, max error 7.5e-8).
fn normal_cdf(z: f64) -> f64 {
    let t    = 1.0 / (1.0 + 0.2316419 * z.abs());
    let poly = t * (0.319381530 + t * (-0.356563782
             + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));
    let phi  = (-z * z * 0.5).exp() / (2.0 * std::f64::consts::PI).sqrt();
    let cdf  = 1.0 - phi * poly;
    if z >= 0.0 { cdf } else { 1.0 - cdf }
}

/// Wilder RSI over the last `prices.len()` values. Returns 50.0 if < 2 values.
fn compute_rsi(prices: &[f64]) -> f64 {
    if prices.len() < 2 {
        return 50.0;
    }
    let n = prices.len().min(14);
    let slice = &prices[prices.len() - n..];
    let mut gains = 0.0f64;
    let mut losses = 0.0f64;
    for w in slice.windows(2) {
        let delta = w[1] - w[0];
        if delta > 0.0 { gains += delta; } else { losses -= delta; }
    }
    let periods = (n - 1) as f64;
    let avg_gain = gains / periods;
    let avg_loss = losses / periods;
    if avg_loss == 0.0 { return 100.0; }
    100.0 - 100.0 / (1.0 + avg_gain / avg_loss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use btcbot_core::{OperationMode, Side};

    fn base_config() -> DecisionConfig {
        DecisionConfig {
            min_remaining_secs:        10.0,
            max_remaining_secs:        30.0,
            max_entry_token_price_yes: 0.90,
            max_entry_token_price_no:  0.90,
            min_entry_token_price:     0.0,
            min_liq_multiplier:        0.0,
            max_spread:                0.0,
            max_quote_skew:            0.10,
            operation_mode:            OperationMode::Degen,
            ..DecisionConfig::default()
        }
    }

    fn snapshot_at(reference_price: f64, remaining_secs: f64, yes: f64, no: f64) -> MarketSnapshot {
        let window_end_ns = 300_000_000_000i64;
        let now_ns = window_end_ns - (remaining_secs * 1e9) as i64;
        MarketSnapshot {
            timestamp_ns:         now_ns,
            market_id:            "test".into(),
            reference_price,
            strike_price:         95_000.0,
            yes_price:            yes,
            no_price:             no,
            window_start_ns:      0,
            window_end_ns,
            momentum_usd_per_sec: 0.0,
            liquidity_yes:        0.0,
            liquidity_no:         0.0,
            volume_5m:            0.0,
            spread:               0.0,
            token_id_yes:         "y".into(),
            token_id_no:          "n".into(),
            neg_risk:             false,
            oi_5m_pct:            0.0,
            price_history:        vec![],
            volume_history:       vec![],
        }
    }

    #[test]
    fn reject_too_much_time_left() {
        let snap = snapshot_at(95_200.0, 60.0, 0.55, 0.45);
        let cfg  = base_config(); // max_remaining=30
        assert!(matches!(evaluate(&snap, &cfg), Evaluation::Reject(RejectReason::TooMuchTimeLeft)));
    }

    #[test]
    fn reject_near_window_end() {
        let snap = snapshot_at(95_200.0, 5.0, 0.55, 0.45);
        let cfg  = base_config(); // min_remaining=10
        assert!(matches!(evaluate(&snap, &cfg), Evaluation::Reject(RejectReason::NearWindowEnd)));
    }

    #[test]
    fn reject_score_neutral_no_history() {
        // reference == strike → window_delta = 0 → no score → neutral
        let snap = snapshot_at(95_000.0, 20.0, 0.50, 0.50);
        let cfg  = base_config();
        assert!(matches!(evaluate(&snap, &cfg), Evaluation::Reject(RejectReason::ScoreNeutral)));
    }

    #[test]
    fn enter_up_strong_window_delta() {
        // BTC rose > 0.10% above strike → score +7 → confidence 1.0 → YES
        let snap = snapshot_at(95_200.0, 20.0, 0.55, 0.45); // +200 on 95000 = +0.21%
        let cfg  = base_config();
        match evaluate(&snap, &cfg) {
            Evaluation::Enter(s) => {
                assert_eq!(s.side, Side::Yes);
                assert!(s.confidence >= 0.9, "expected high confidence, got {}", s.confidence);
            }
            other => panic!("expected Enter, got {:?}", other),
        }
    }

    #[test]
    fn enter_down_strong_window_delta() {
        let snap = snapshot_at(94_800.0, 20.0, 0.45, 0.55); // -200 on 95000 = -0.21%
        let cfg  = base_config();
        match evaluate(&snap, &cfg) {
            Evaluation::Enter(s) => assert_eq!(s.side, Side::No),
            other => panic!("expected Enter, got {:?}", other),
        }
    }

    #[test]
    fn reject_token_too_expensive() {
        let mut cfg = base_config();
        cfg.max_entry_token_price_yes = 0.50;
        let snap = snapshot_at(95_200.0, 20.0, 0.60, 0.40);
        assert!(matches!(evaluate(&snap, &cfg), Evaluation::Reject(RejectReason::TokenTooExpensive)));
    }

    #[test]
    fn reject_low_confidence_safe_mode() {
        let mut cfg = base_config();
        cfg.operation_mode = OperationMode::Safe; // needs confidence >= 0.30
        // reference=95015 → 15/95000 = 0.016% < 0.02 → score=0 → neutral
        let snap2 = snapshot_at(95_015.0, 20.0, 0.50, 0.50);
        assert!(matches!(evaluate(&snap2, &cfg), Evaluation::Reject(_)));
    }
}
