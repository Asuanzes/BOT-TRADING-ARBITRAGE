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

/// How aggressively to require confidence before entering a trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OperationMode {
    Safe,
    #[default]
    Aggressive,
    Degen,
}

impl OperationMode {
    pub fn min_confidence(self) -> f64 {
        match self {
            OperationMode::Safe       => 0.30,
            OperationMode::Aggressive => 0.20,
            OperationMode::Degen      => 0.10,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OperationMode::Safe       => "safe",
            OperationMode::Aggressive => "aggressive",
            OperationMode::Degen      => "degen",
        }
    }
}

/// Configuration for one market loaded from config/markets.toml.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MarketConfig {
    pub id:                 String,
    pub symbol:             String,
    pub window_secs:        u64,
    /// Minimum USD diff between reference and strike to consider a trade (0 = disabled).
    #[serde(default)]
    pub min_price_diff_usd: f64,
    /// Seconds into the window before entering (0 = no delay).
    #[serde(default)]
    pub entry_delay_secs:   f64,
    /// Minimum seconds remaining in window to allow entry.
    #[serde(default)]
    pub min_remaining_secs: f64,
    /// Maximum seconds remaining in window to allow entry (0 = disabled).
    #[serde(default)]
    pub max_remaining_secs: f64,
    /// Reject entry if the YES token's quoted price exceeds this value.
    #[serde(default = "default_max_entry_token_price")]
    pub max_entry_token_price_yes: f64,
    /// Reject entry if the NO token's quoted price exceeds this value.
    #[serde(default = "default_max_entry_token_price")]
    pub max_entry_token_price_no: f64,
    /// Reject entry if the target token price is below this (near-zero lottery skip).
    #[serde(default)]
    pub min_entry_token_price: f64,
    /// Reject entry if underlying momentum (USD/s) goes against the direction.
    #[serde(default)]
    pub momentum_reject_usd_per_sec: f64,
    /// Minimum 5-minute volume to consider a trade (0 = disabled).
    #[serde(default)]
    pub min_volume_5m: f64,
    /// Minimum liquidity multiplier relative to entry size (0 = disabled).
    #[serde(default)]
    pub min_liq_multiplier: f64,
    /// Maximum bid/ask spread to tolerate (0 = disabled).
    #[serde(default)]
    pub max_spread: f64,
    /// Maximum skew of yes+no away from 1.0 before quotes are considered stale (0 = disabled).
    #[serde(default)]
    pub max_quote_skew: f64,
    /// Minimum expected edge to enter (0 = disabled).
    #[serde(default)]
    pub min_entry_edge: f64,
    /// Volume per sqrt-second threshold for momentum fast-path (0 = disabled).
    #[serde(default)]
    pub vol_usd_per_sqrts: f64,
    /// Strong momentum threshold USD/s (0 = disabled).
    #[serde(default)]
    pub strong_momentum_usd_per_sec: f64,
    /// Lower price diff threshold when strong momentum amplifies direction (0 = disabled).
    #[serde(default)]
    pub min_price_diff_usd_strong: f64,
    /// OI contrarian filter pct (0 = disabled).
    #[serde(default)]
    pub oi_contrarian_pct: f64,
    /// Fade extreme diff above this USD (0 = disabled).
    #[serde(default)]
    pub fade_extreme_diff_usd: f64,
    /// Dead-zone start seconds into window (0 = disabled).
    #[serde(default)]
    pub dead_zone_start_secs: f64,
    /// Dead-zone end seconds into window (0 = disabled).
    #[serde(default)]
    pub dead_zone_end_secs: f64,
    /// How aggressively to require confidence before entering.
    #[serde(default)]
    pub operation_mode: OperationMode,
}

fn default_max_entry_token_price() -> f64 { 0.80 }

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
    /// Instantaneous velocity of the underlying in USD/s.
    #[serde(default)]
    pub momentum_usd_per_sec: f64,
    /// Best ask liquidity on the YES side (USDC).
    #[serde(default)]
    pub liquidity_yes: f64,
    /// Best ask liquidity on the NO side (USDC).
    #[serde(default)]
    pub liquidity_no: f64,
    /// 5-minute total volume (USDC).
    #[serde(default)]
    pub volume_5m: f64,
    /// Current bid/ask spread.
    #[serde(default)]
    pub spread: f64,
    /// YES token ID (for order placement).
    #[serde(default)]
    pub token_id_yes: String,
    /// NO token ID (for order placement).
    #[serde(default)]
    pub token_id_no: String,
    /// Whether this is a neg-risk CTF market.
    #[serde(default)]
    pub neg_risk: bool,
    /// Open interest change over the last 5 minutes as a percentage of total OI.
    #[serde(default)]
    pub oi_5m_pct: f64,
    /// Last N one-minute BTC closes, oldest first. Populated by the feed.
    /// Empty until the first minute elapses. Used by decision for RSI + micro-momentum.
    #[serde(default)]
    pub price_history: Vec<f64>,
    /// Last M 5-minute total volume samples, oldest first. Used for volume-surge detection.
    #[serde(default)]
    pub volume_history: Vec<f64>,
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
    /// Maximum seconds remaining in window to allow entry (0 = no cap)
    pub max_remaining_secs: f64,
    /// Reject entry if YES token price is above this.
    pub max_entry_token_price_yes: f64,
    /// Reject entry if NO token price is above this.
    pub max_entry_token_price_no: f64,
    /// Reject entry if target token price is below this (near-zero lottery skip).
    pub min_entry_token_price: f64,
    /// Reject entry if underlying momentum (USD/s) goes against the direction
    pub momentum_reject_usd_per_sec: f64,
    /// Minimum 5-minute volume.
    pub min_volume_5m: f64,
    /// Minimum liquidity multiplier.
    pub min_liq_multiplier: f64,
    /// Maximum spread.
    pub max_spread: f64,
    /// Maximum quote skew (yes + no - 1.0).
    pub max_quote_skew: f64,
    /// Minimum edge.
    pub min_entry_edge: f64,
    /// Volume per sqrt-second threshold.
    pub vol_usd_per_sqrts: f64,
    /// Strong momentum threshold USD/s.
    pub strong_momentum_usd_per_sec: f64,
    /// Lower price diff threshold for strong momentum.
    pub min_price_diff_usd_strong: f64,
    /// OI contrarian pct.
    pub oi_contrarian_pct: f64,
    /// Fade extreme diff USD.
    pub fade_extreme_diff_usd: f64,
    /// Dead-zone start secs.
    pub dead_zone_start_secs: f64,
    /// Dead-zone end secs.
    pub dead_zone_end_secs: f64,
    /// Entry size for liquidity comparisons.
    pub entry_size_usdc: f64,
    /// Operation mode.
    pub operation_mode: OperationMode,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self {
            min_price_diff_usd: 0.0,
            entry_delay_secs: 0.0,
            min_remaining_secs: 10.0,
            max_remaining_secs: 30.0,
            max_entry_token_price_yes: 0.90,
            max_entry_token_price_no: 0.90,
            min_entry_token_price: 0.0,
            momentum_reject_usd_per_sec: 0.0,
            min_volume_5m: 0.0,
            min_liq_multiplier: 0.0,
            max_spread: 0.0,
            max_quote_skew: 0.10,
            min_entry_edge: 0.0,
            vol_usd_per_sqrts: 0.0,
            strong_momentum_usd_per_sec: 0.0,
            min_price_diff_usd_strong: 0.0,
            oi_contrarian_pct: 0.0,
            fade_extreme_diff_usd: 0.0,
            dead_zone_start_secs: 0.0,
            dead_zone_end_secs: 0.0,
            entry_size_usdc: 150.0,
            operation_mode: OperationMode::Aggressive,
        }
    }
}
