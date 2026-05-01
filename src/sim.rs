//! SimAccount: cash / realized PnL bookkeeping for simulation mode.
//! Writes logs/equity.csv (per-trade) and logs/stats.json (running totals) on every close.

use btcbot_core::Side;
use chrono::Utc;
use serde::Serialize;

const EQUITY_CSV: &str = "logs/equity.csv";
const STATS_JSON: &str = "logs/stats.json";
const RECENT_CAP: usize = 20;

#[derive(Serialize, Clone)]
struct TradeRecord {
    ts:           String,
    market:       String,
    side:         String,
    entry:        f64,
    exit:         f64,
    size_usdc:    f64,
    pnl_usd:      f64,
    pnl_pct:      f64,
    balance:      f64,
    close_reason: String,
}

#[derive(Serialize)]
struct StatsSnapshot {
    updated_at:       String,
    balance:          f64,
    starting_balance: f64,
    realized_pnl:     f64,
    roi_pct:          f64,
    win_rate_pct:     f64,
    wins:             u32,
    losses:           u32,
    trades:           u32,
    streak:           i32,
    recent:           Vec<TradeRecord>,
}

pub struct SimAccount {
    cash:             f64,
    starting_balance: f64,
    open_notional:    f64,
    realized_pnl:     f64,
    wins:             u32,
    losses:           u32,
    streak:           i32,
    recent:           Vec<TradeRecord>,
    csv_initialized:  bool,
}

impl SimAccount {
    pub fn new(starting_balance: f64) -> Self {
        Self {
            cash: starting_balance,
            starting_balance,
            open_notional: 0.0,
            realized_pnl: 0.0,
            wins: 0,
            losses: 0,
            streak: 0,
            recent: Vec::new(),
            csv_initialized: false,
        }
    }

    /// Reserve `size_usdc` from cash when opening a position.
    pub fn try_open(&mut self, _market_id: &str, size_usdc: f64) {
        let deducted = size_usdc.min(self.cash);
        self.cash -= deducted;
        self.open_notional += deducted;
    }

    /// Record a closed position, persist to CSV + JSON, and return PnL in USD.
    pub fn record_close(
        &mut self,
        market_id: &str,
        side: Side,
        entry_price: f64,
        exit_price: f64,
        size_usdc: f64,
        close_reason: &str,
    ) -> f64 {
        let notional = size_usdc.min(self.open_notional);
        self.open_notional -= notional;

        let pnl = if entry_price > 0.0 {
            (exit_price - entry_price) / entry_price * size_usdc
        } else {
            0.0
        };
        let pnl_pct = if entry_price > 0.0 { (exit_price - entry_price) / entry_price } else { 0.0 };

        self.cash += notional + pnl;
        self.realized_pnl += pnl;

        if pnl >= 0.0 {
            self.wins  += 1;
            self.streak = if self.streak >= 0 { self.streak + 1 } else { 1 };
        } else {
            self.losses += 1;
            self.streak = if self.streak <= 0 { self.streak - 1 } else { -1 };
        }

        let record = TradeRecord {
            ts:           Utc::now().to_rfc3339(),
            market:       market_id.to_string(),
            side:         format!("{side:?}"),
            entry:        entry_price,
            exit:         exit_price,
            size_usdc,
            pnl_usd:      pnl,
            pnl_pct,
            balance:      self.cash,
            close_reason: close_reason.to_string(),
        };

        self.recent.insert(0, record.clone());
        if self.recent.len() > RECENT_CAP { self.recent.truncate(RECENT_CAP); }

        self.write_csv(&record);
        self.write_stats();

        pnl
    }

    fn write_csv(&mut self, record: &TradeRecord) {
        use std::fs::OpenOptions;
        use std::io::Write as _;

        let need_header = !self.csv_initialized && {
            let path = std::path::Path::new(EQUITY_CSV);
            !path.exists() || std::fs::metadata(path).map(|m| m.len() == 0).unwrap_or(true)
        };
        self.csv_initialized = true;

        let mut file = match OpenOptions::new().create(true).append(true).open(EQUITY_CSV) {
            Ok(f)  => f,
            Err(e) => { tracing::warn!("sim: no se puede escribir equity.csv: {e}"); return; }
        };

        if need_header {
            let _ = writeln!(file,
                "timestamp_iso,trade_n,market,side,entry_price,exit_price,\
                 size_usdc,pnl_usd,pnl_pct,realized_pnl,balance,wins,losses");
        }

        let n = self.wins + self.losses;
        let _ = writeln!(file,
            "{},{},{},{},{:.6},{:.6},{:.2},{:.4},{:.6},{:.4},{:.2},{},{}",
            record.ts, n, record.market, record.side,
            record.entry, record.exit, record.size_usdc,
            record.pnl_usd, record.pnl_pct, self.realized_pnl,
            self.cash, self.wins, self.losses,
        );
    }

    fn write_stats(&self) {
        let trades = self.wins + self.losses;
        let win_rate = if trades > 0 { self.wins as f64 / trades as f64 * 100.0 } else { 0.0 };
        let roi = if self.starting_balance > 0.0 {
            (self.cash - self.starting_balance) / self.starting_balance * 100.0
        } else { 0.0 };

        let snap = StatsSnapshot {
            updated_at:       Utc::now().to_rfc3339(),
            balance:          self.cash,
            starting_balance: self.starting_balance,
            realized_pnl:     self.realized_pnl,
            roi_pct:          roi,
            win_rate_pct:     win_rate,
            wins:             self.wins,
            losses:           self.losses,
            trades,
            streak:           self.streak,
            recent:           self.recent.clone(),
        };

        match serde_json::to_string(&snap) {
            Ok(json) => {
                if let Err(e) = std::fs::write(STATS_JSON, json) {
                    tracing::warn!("sim: no se puede escribir stats.json: {e}");
                }
            }
            Err(e) => tracing::warn!("sim: error serializando stats: {e}"),
        }
    }

    pub fn balance(&self) -> f64      { self.cash }
    pub fn realized_pnl(&self) -> f64 { self.realized_pnl }
    pub fn wins(&self) -> u32         { self.wins }
    pub fn losses(&self) -> u32       { self.losses }
}
