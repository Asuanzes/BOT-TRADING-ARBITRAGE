use serde::{Deserialize, Serialize};

pub const BTC_5M_UPDOWN: &str = "BTC_5M_UPDOWN";

/// Bot execution mode. Set via `mode` in config/markets.toml.
/// Simulation: no orders are sent; all activity is logged only.
/// Live: orders are submitted to Polymarket (execution layer required).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    Simulation,
    Live,
}

/// Configuration for one market loaded from config/markets.toml.
/// `window_secs` and `symbol` are stored for documentation and future use
/// (e.g. multi-market routing, feed validation); decision logic only uses
/// the three threshold fields below.
/// To add a second market, append another [[markets]] block to the TOML file
/// and spawn a dedicated feed + decision task keyed on `id`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MarketConfig {
    pub id:                 String,
    pub symbol:             String,
    pub window_secs:        u64,
    pub min_price_diff_usd: f64,
    pub entry_delay_secs:   f64,
    pub min_remaining_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub timestamp_ns: i64,
    pub market_id: String,
    /// Current underlying price (e.g. BTC/USD)
    pub reference_price: f64,
    /// Price the market resolves against
    pub strike_price: f64,
    pub yes_price: f64,
    pub no_price: f64,
    pub window_start_ns: i64,
    pub window_end_ns: i64,
}

impl MarketSnapshot {
    pub fn elapsed_secs(&self) -> f64 {
        (self.timestamp_ns - self.window_start_ns) as f64 / 1e9
    }

    pub fn remaining_secs(&self) -> f64 {
        (self.window_end_ns - self.timestamp_ns) as f64 / 1e9
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Yes,
    No,
}

impl Direction {
    pub fn to_side(self) -> Side {
        match self {
            Direction::Up => Side::Yes,
            Direction::Down => Side::No,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub direction: Direction,
    pub side: Side,
    pub token_price: f64,
    pub confidence: f64,
    pub entry_at_ns: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderKind {
    Market,
    Limit { price: f64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub market_id: String,
    pub side: Side,
    pub kind: OrderKind,
    pub size_usdc: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub market_id: String,
    pub side: Side,
    pub entry_price: f64,
    pub size_usdc: f64,
    pub entry_time_ns: i64,
}

impl Position {
    pub fn unrealized_pnl(&self, current_price: f64) -> f64 {
        let tokens = self.size_usdc / self.entry_price;
        tokens * (current_price - self.entry_price)
    }

    pub fn pnl_pct(&self, current_price: f64) -> f64 {
        (current_price - self.entry_price) / self.entry_price
    }
}

#[derive(Debug, Clone)]
pub struct DecisionConfig {
    /// Minimum USD diff between reference and strike to consider a trade
    pub min_price_diff_usd: f64,
    /// Seconds into the window before entering
    pub entry_delay_secs: f64,
    /// Minimum seconds remaining in window to allow entry
    pub min_remaining_secs: f64,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self {
            min_price_diff_usd: 50.0,
            entry_delay_secs: 30.0,
            min_remaining_secs: 60.0,
        }
    }
}
