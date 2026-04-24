use btcbot_core::{DecisionConfig, Direction, MarketConfig, MarketSnapshot, Signal};

/// Builds a DecisionConfig from any MarketConfig entry.
/// This module is market-agnostic: the same thresholds apply to any binary up/down market.
/// Window duration is enforced by the feed layer and is not part of DecisionConfig.
pub fn decision_config_for(cfg: &MarketConfig) -> DecisionConfig {
    DecisionConfig {
        min_price_diff_usd:          cfg.min_price_diff_usd,
        entry_delay_secs:            cfg.entry_delay_secs,
        min_remaining_secs:          cfg.min_remaining_secs,
        max_entry_token_price:       cfg.max_entry_token_price,
        momentum_reject_usd_per_sec: cfg.momentum_reject_usd_per_sec,
    }
}

/// Evaluates a snapshot against the given thresholds.
/// Market-agnostic: works for any binary up/down market whose snapshot is correctly populated.
/// Returns None if timing or price-diff conditions are not met.
pub fn evaluate(snapshot: &MarketSnapshot, config: &DecisionConfig) -> Option<Signal> {
    if snapshot.elapsed_secs() < config.entry_delay_secs {
        return None;
    }
    if snapshot.remaining_secs() < config.min_remaining_secs {
        return None;
    }

    let diff = snapshot.reference_price - snapshot.strike_price;

    let direction = if diff > config.min_price_diff_usd {
        Direction::Up
    } else if diff < -config.min_price_diff_usd {
        Direction::Down
    } else {
        return None;
    };

    // Momentum filter: skip when the underlying is actively moving against the
    // diff-derived direction. Prevents entering right after a spike that's reverting.
    if config.momentum_reject_usd_per_sec > 0.0 {
        let contradicts = match direction {
            Direction::Up   => snapshot.momentum_usd_per_sec <= -config.momentum_reject_usd_per_sec,
            Direction::Down => snapshot.momentum_usd_per_sec >=  config.momentum_reject_usd_per_sec,
        };
        if contradicts { return None; }
    }

    let (token_price, confidence) = match direction {
        Direction::Up => (snapshot.yes_price, confidence(diff, config.min_price_diff_usd)),
        Direction::Down => (snapshot.no_price, confidence(-diff, config.min_price_diff_usd)),
    };

    // Skip entries where the target token is already trading near 1.00:
    // the remaining upside is too small to justify the downside risk.
    if token_price > config.max_entry_token_price {
        return None;
    }

    Some(Signal {
        direction,
        side: direction.to_side(),
        token_price,
        confidence,
        entry_at_ns: snapshot.timestamp_ns,
    })
}

/// Scales excess diff to [0.0, 1.0], saturating at 3× the threshold.
fn confidence(excess_usd: f64, threshold: f64) -> f64 {
    (excess_usd / (threshold * 3.0)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use btcbot_core::Side;

    fn snapshot(reference_price: f64, elapsed_secs: f64) -> MarketSnapshot {
        snapshot_full(reference_price, elapsed_secs, 0.5, 0.5, 0.0)
    }

    fn snapshot_with_token_prices(
        reference_price: f64,
        elapsed_secs: f64,
        yes_price: f64,
        no_price: f64,
    ) -> MarketSnapshot {
        snapshot_full(reference_price, elapsed_secs, yes_price, no_price, 0.0)
    }

    fn snapshot_full(
        reference_price: f64,
        elapsed_secs: f64,
        yes_price: f64,
        no_price: f64,
        momentum_usd_per_sec: f64,
    ) -> MarketSnapshot {
        let window_end_ns = 300_000_000_000i64;
        let now = (elapsed_secs * 1e9) as i64;
        MarketSnapshot {
            timestamp_ns: now,
            market_id: "test".into(),
            reference_price,
            strike_price: 65_000.0,
            yes_price,
            no_price,
            window_start_ns: 0,
            window_end_ns,
            momentum_usd_per_sec,
        }
    }

    #[test]
    fn no_trade_before_entry_delay() {
        assert!(evaluate(&snapshot(65_200.0, 10.0), &DecisionConfig::default()).is_none());
    }

    #[test]
    fn no_trade_diff_too_small() {
        assert!(evaluate(&snapshot(65_020.0, 60.0), &DecisionConfig::default()).is_none());
    }

    #[test]
    fn up_signal_above_strike() {
        let sig = evaluate(&snapshot(65_100.0, 60.0), &DecisionConfig::default()).unwrap();
        assert_eq!(sig.direction, Direction::Up);
        assert_eq!(sig.side, Side::Yes);
        assert!(sig.confidence > 0.0 && sig.confidence <= 1.0);
    }

    #[test]
    fn down_signal_below_strike() {
        let sig = evaluate(&snapshot(64_900.0, 60.0), &DecisionConfig::default()).unwrap();
        assert_eq!(sig.direction, Direction::Down);
        assert_eq!(sig.side, Side::No);
    }

    #[test]
    fn no_trade_near_window_end() {
        // 260s elapsed → 40s remaining < min_remaining_secs(60)
        assert!(evaluate(&snapshot(65_200.0, 260.0), &DecisionConfig::default()).is_none());
    }

    #[test]
    fn no_entry_when_yes_token_already_expensive() {
        // Up signal (diff +100) but YES quote 0.90 > max_entry_token_price(0.80) → skip.
        let snap = snapshot_with_token_prices(65_100.0, 60.0, 0.90, 0.10);
        assert!(evaluate(&snap, &DecisionConfig::default()).is_none());
    }

    #[test]
    fn no_entry_when_no_token_already_expensive() {
        // Down signal (diff -100) but NO quote 0.92 > 0.80 → skip.
        let snap = snapshot_with_token_prices(64_900.0, 60.0, 0.08, 0.92);
        assert!(evaluate(&snap, &DecisionConfig::default()).is_none());
    }

    #[test]
    fn entry_allowed_when_target_token_still_cheap() {
        // Up signal, YES still 0.55 → entry allowed.
        let snap = snapshot_with_token_prices(65_100.0, 60.0, 0.55, 0.45);
        let sig = evaluate(&snap, &DecisionConfig::default()).unwrap();
        assert_eq!(sig.direction, Direction::Up);
        assert_eq!(sig.token_price, 0.55);
    }

    #[test]
    fn momentum_against_up_direction_rejects_entry() {
        // Diff +100 → Up; but underlying falling at -3 USD/s (> 2.0 threshold) → skip.
        let snap = snapshot_full(65_100.0, 60.0, 0.55, 0.45, -3.0);
        assert!(evaluate(&snap, &DecisionConfig::default()).is_none());
    }

    #[test]
    fn momentum_against_down_direction_rejects_entry() {
        // Diff -100 → Down; but underlying rising at +3 USD/s → skip.
        let snap = snapshot_full(64_900.0, 60.0, 0.45, 0.55, 3.0);
        assert!(evaluate(&snap, &DecisionConfig::default()).is_none());
    }

    #[test]
    fn momentum_in_favor_allows_entry() {
        // Up signal and momentum +5 USD/s → pass through.
        let snap = snapshot_full(65_100.0, 60.0, 0.55, 0.45, 5.0);
        let sig = evaluate(&snap, &DecisionConfig::default()).unwrap();
        assert_eq!(sig.direction, Direction::Up);
    }

    #[test]
    fn momentum_below_threshold_does_not_reject() {
        // Up signal, momentum -1.0 USD/s — below |2.0| threshold → not a veto.
        let snap = snapshot_full(65_100.0, 60.0, 0.55, 0.45, -1.0);
        assert!(evaluate(&snap, &DecisionConfig::default()).is_some());
    }

    #[test]
    fn momentum_filter_disabled_when_threshold_zero() {
        // Strong contradicting momentum but threshold = 0 disables the check.
        let snap = snapshot_full(65_100.0, 60.0, 0.55, 0.45, -50.0);
        let cfg = DecisionConfig { momentum_reject_usd_per_sec: 0.0, ..DecisionConfig::default() };
        assert!(evaluate(&snap, &cfg).is_some());
    }
}
