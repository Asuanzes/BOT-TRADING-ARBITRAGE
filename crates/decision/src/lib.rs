use btcbot_core::{DecisionConfig, Direction, MarketConfig, MarketSnapshot, Signal};

/// Builds a DecisionConfig from any MarketConfig entry.
/// This module is market-agnostic: the same thresholds apply to any binary up/down market.
/// Window duration is enforced by the feed layer and is not part of DecisionConfig.
pub fn decision_config_for(cfg: &MarketConfig) -> DecisionConfig {
    DecisionConfig {
        min_price_diff_usd: cfg.min_price_diff_usd,
        entry_delay_secs:   cfg.entry_delay_secs,
        min_remaining_secs: cfg.min_remaining_secs,
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

    let (token_price, confidence) = match direction {
        Direction::Up => (snapshot.yes_price, confidence(diff, config.min_price_diff_usd)),
        Direction::Down => (snapshot.no_price, confidence(-diff, config.min_price_diff_usd)),
    };

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
        let window_end_ns = 300_000_000_000i64;
        let now = (elapsed_secs * 1e9) as i64;
        MarketSnapshot {
            timestamp_ns: now,
            market_id: "test".into(),
            reference_price,
            strike_price: 65_000.0,
            yes_price: 0.5,
            no_price: 0.5,
            window_start_ns: 0,
            window_end_ns,
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
}
