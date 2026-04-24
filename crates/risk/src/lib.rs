use btcbot_core::Position;

#[derive(Debug, Clone)]
pub struct RiskConfig {
    pub take_profit_pct: f64,
    pub stop_loss_pct: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            take_profit_pct: 0.50,
            stop_loss_pct: 0.20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    TakeProfit,
    StopLoss,
    Timeout,
    Hold,
}

/// Returns whether and why a position should be closed.
/// Timeout is checked first so positions always close at window end.
pub fn should_close(
    position: &Position,
    current_price: f64,
    remaining_secs: f64,
    config: &RiskConfig,
) -> CloseReason {
    if remaining_secs <= 0.0 {
        return CloseReason::Timeout;
    }
    let pnl = position.pnl_pct(current_price);
    if pnl >= config.take_profit_pct {
        return CloseReason::TakeProfit;
    }
    if pnl <= -config.stop_loss_pct {
        return CloseReason::StopLoss;
    }
    CloseReason::Hold
}

#[cfg(test)]
mod tests {
    use super::*;
    use btcbot_core::{Position, Side};

    fn position(entry_price: f64) -> Position {
        Position {
            market_id: "test".into(),
            side: Side::Yes,
            entry_price,
            size_usdc: 10.0,
            entry_time_ns: 0,
        }
    }

    #[test]
    fn take_profit_at_50pct() {
        let p = position(0.40);
        // 0.62 → pnl = 0.55 > 0.50, avoids IEEE 754 edge at exactly 0.60
        assert_eq!(
            should_close(&p, 0.62, 120.0, &RiskConfig::default()),
            CloseReason::TakeProfit
        );
    }

    #[test]
    fn stop_loss_at_20pct() {
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.32, 120.0, &RiskConfig::default()),
            CloseReason::StopLoss
        );
    }

    #[test]
    fn timeout_when_no_time_remaining() {
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.40, 0.0, &RiskConfig::default()),
            CloseReason::Timeout
        );
    }

    #[test]
    fn timeout_takes_priority_over_tp() {
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.70, 0.0, &RiskConfig::default()),
            CloseReason::Timeout
        );
    }

    #[test]
    fn hold_within_bounds() {
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.42, 120.0, &RiskConfig::default()),
            CloseReason::Hold
        );
    }
}
