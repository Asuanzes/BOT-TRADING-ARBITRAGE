use btcbot_core::{Position, Side};

#[derive(Debug, Clone)]
pub struct RiskConfig {
    pub take_profit_pct: f64,
    pub stop_loss_pct: f64,
    /// Arm the trailing stop once pnl has reached this pct.
    pub trail_arm_pct: f64,
    /// Close if pnl falls back to this fraction of the peak after arming.
    pub trail_giveback_pct: f64,
    /// Close if the underlying reverses past this USD diff against the position's direction.
    pub reversal_diff_usd: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            take_profit_pct: 0.50,
            stop_loss_pct: 0.20,
            trail_arm_pct: 0.30,
            trail_giveback_pct: 0.60,
            reversal_diff_usd: 40.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    TakeProfit,
    StopLoss,
    TrailingStop,
    Reversal,
    Timeout,
    Hold,
}

/// Returns whether and why a position should be closed.
///
/// `underlying_diff_usd` = reference_price − strike_price at the current snapshot.
/// `peak_pnl_pct` = high-water mark of pnl_pct since the position was opened (monotonic).
///
/// Order of checks is load-bearing:
///   1. Timeout — always wins at window end.
///   2. Reversal — cut quickly when the underlying turns against us, before the token quote catches up.
///   3. Take-profit — lock in a clean win.
///   4. Trailing stop — only possible once armed (peak ≥ trail_arm_pct).
///   5. Stop-loss — last resort on token-quote drawdown.
pub fn should_close(
    position: &Position,
    current_price: f64,
    remaining_secs: f64,
    underlying_diff_usd: f64,
    peak_pnl_pct: f64,
    config: &RiskConfig,
) -> CloseReason {
    if remaining_secs <= 0.0 {
        return CloseReason::Timeout;
    }

    let reversed_against_us = match position.side {
        Side::Yes => underlying_diff_usd < -config.reversal_diff_usd,
        Side::No  => underlying_diff_usd >  config.reversal_diff_usd,
    };
    if reversed_against_us {
        return CloseReason::Reversal;
    }

    let pnl = position.pnl_pct(current_price);
    if pnl >= config.take_profit_pct {
        return CloseReason::TakeProfit;
    }

    if peak_pnl_pct >= config.trail_arm_pct
        && pnl <= peak_pnl_pct * config.trail_giveback_pct
    {
        return CloseReason::TrailingStop;
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
        position_with_side(entry_price, Side::Yes)
    }

    fn position_with_side(entry_price: f64, side: Side) -> Position {
        Position {
            market_id: "test".into(),
            side,
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
            should_close(&p, 0.62, 120.0, 0.0, 0.55, &RiskConfig::default()),
            CloseReason::TakeProfit
        );
    }

    #[test]
    fn stop_loss_at_20pct() {
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.32, 120.0, 0.0, 0.0, &RiskConfig::default()),
            CloseReason::StopLoss
        );
    }

    #[test]
    fn timeout_when_no_time_remaining() {
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.40, 0.0, 0.0, 0.0, &RiskConfig::default()),
            CloseReason::Timeout
        );
    }

    #[test]
    fn timeout_takes_priority_over_tp() {
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.70, 0.0, 0.0, 0.75, &RiskConfig::default()),
            CloseReason::Timeout
        );
    }

    #[test]
    fn hold_within_bounds() {
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.42, 120.0, 0.0, 0.05, &RiskConfig::default()),
            CloseReason::Hold
        );
    }

    #[test]
    fn trailing_stop_after_peak_giveback() {
        // entry 0.40, peaked at +40% (→0.56), now back to +20% (→0.48):
        // giveback threshold = 0.40 * 0.60 = 0.24 → pnl 0.20 ≤ 0.24 → TrailingStop.
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.48, 120.0, 0.0, 0.40, &RiskConfig::default()),
            CloseReason::TrailingStop
        );
    }

    #[test]
    fn trailing_not_armed_before_arm_pct() {
        // peak 0.20 < arm 0.30 → trailing must not fire even with full giveback.
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.40, 120.0, 0.0, 0.20, &RiskConfig::default()),
            CloseReason::Hold
        );
    }

    #[test]
    fn reversal_yes_when_underlying_drops() {
        // Side::Yes, diff = -50 < -40 (reversal_diff_usd) → Reversal.
        let p = position_with_side(0.40, Side::Yes);
        assert_eq!(
            should_close(&p, 0.40, 120.0, -50.0, 0.0, &RiskConfig::default()),
            CloseReason::Reversal
        );
    }

    #[test]
    fn reversal_no_when_underlying_rises() {
        // Side::No, diff = +50 > +40 → Reversal.
        let p = position_with_side(0.40, Side::No);
        assert_eq!(
            should_close(&p, 0.40, 120.0, 50.0, 0.0, &RiskConfig::default()),
            CloseReason::Reversal
        );
    }

    #[test]
    fn reversal_does_not_trigger_when_in_favor() {
        // Side::Yes with big positive diff → no reversal.
        let p = position_with_side(0.40, Side::Yes);
        assert_eq!(
            should_close(&p, 0.42, 120.0, 200.0, 0.05, &RiskConfig::default()),
            CloseReason::Hold
        );
    }

    #[test]
    fn tp_wins_over_trailing_when_both_fire() {
        // pnl 0.55 triggers TP; even if trailing math would also match, TP is checked first.
        let p = position(0.40);
        assert_eq!(
            should_close(&p, 0.62, 120.0, 0.0, 0.55, &RiskConfig::default()),
            CloseReason::TakeProfit
        );
    }
}
