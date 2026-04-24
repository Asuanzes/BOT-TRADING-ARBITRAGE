//! Execution layer: the only place that talks to the exchange.
//!
//! In Simulation mode, orders are synthesized as immediate fills at the quoted price.
//! In Live mode, orders should be submitted to Polymarket CLOB — TODO, not yet wired.
//! Keeping this crate side-effect-free (no logging of business events) lets the
//! orchestration layer (main.rs) decide what to surface to operators.

use anyhow::{bail, Result};
use btcbot_core::{RunMode, Side};

/// Outcome of an order submission. In Simulation this is synthetic;
/// in Live it will reflect the exchange's fill report.
#[derive(Debug, Clone)]
pub struct Fill {
    /// Actual token price obtained. In Simulation equals the quoted price.
    pub token_price: f64,
    /// USDC notional that was filled.
    pub size_usdc: f64,
    /// Number of tokens bought or sold (size_usdc / token_price).
    pub size_tokens: f64,
    pub status: FillStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillStatus {
    /// Order accepted by the simulated book without touching a real exchange.
    Simulated,
    /// Order filled in full by the live exchange.
    Filled,
}

/// Submit a BUY for the given `token_side` to open a position.
/// `quoted_price` is the latest observed buy-side CLOB price used to size the fill.
pub async fn place_entry_order(
    mode: &RunMode,
    _market_id: &str,
    _token_side: Side,
    size_usdc: f64,
    quoted_price: f64,
) -> Result<Fill> {
    match mode {
        RunMode::Simulation => Ok(simulated_fill(size_usdc, quoted_price)),
        RunMode::Live => {
            // TODO: POST /order to Polymarket CLOB with BUY on the token_id that maps to
            // `token_side` (Yes or No). Needs signed payload with API creds — not wired yet.
            bail!("live entry execution not implemented");
        }
    }
}

/// Submit a SELL for the given `token_side` to close an existing position.
/// `size_tokens` is the token amount to unwind, `quoted_price` the latest CLOB quote.
pub async fn place_close_order(
    mode: &RunMode,
    _market_id: &str,
    _token_side: Side,
    size_tokens: f64,
    quoted_price: f64,
) -> Result<Fill> {
    match mode {
        RunMode::Simulation => {
            let size_usdc = size_tokens * quoted_price;
            Ok(Fill {
                token_price: quoted_price,
                size_usdc,
                size_tokens,
                status: FillStatus::Simulated,
            })
        }
        RunMode::Live => {
            // TODO: POST /order to Polymarket CLOB with SELL on the same token_id,
            // amount = size_tokens. Needs signed payload. Not wired yet.
            bail!("live close execution not implemented");
        }
    }
}

fn simulated_fill(size_usdc: f64, quoted_price: f64) -> Fill {
    // Guard against a zero quote that would produce an infinite token count.
    let tokens = if quoted_price > 0.0 { size_usdc / quoted_price } else { 0.0 };
    Fill {
        token_price: quoted_price,
        size_usdc,
        size_tokens: tokens,
        status: FillStatus::Simulated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sim_entry_fills_at_quoted_price() {
        let fill = place_entry_order(&RunMode::Simulation, "m", Side::Yes, 10.0, 0.40)
            .await.unwrap();
        assert_eq!(fill.token_price, 0.40);
        assert_eq!(fill.size_usdc, 10.0);
        assert!((fill.size_tokens - 25.0).abs() < 1e-9);
        assert_eq!(fill.status, FillStatus::Simulated);
    }

    #[tokio::test]
    async fn sim_close_computes_notional_from_tokens() {
        let fill = place_close_order(&RunMode::Simulation, "m", Side::Yes, 25.0, 0.60)
            .await.unwrap();
        assert_eq!(fill.token_price, 0.60);
        assert!((fill.size_usdc - 15.0).abs() < 1e-9);
        assert_eq!(fill.size_tokens, 25.0);
    }

    #[tokio::test]
    async fn live_entry_returns_error_until_implemented() {
        assert!(
            place_entry_order(&RunMode::Live, "m", Side::Yes, 10.0, 0.40)
                .await.is_err()
        );
    }

    #[tokio::test]
    async fn live_close_returns_error_until_implemented() {
        assert!(
            place_close_order(&RunMode::Live, "m", Side::Yes, 25.0, 0.60)
                .await.is_err()
        );
    }
}
