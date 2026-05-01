mod config;
mod sim;

use btcbot_core::{Direction, MarketSnapshot, Position, Side};
use chrono::Utc;
use decision::Evaluation;
use risk::{CloseReason, RiskConfig};
use sim::SimAccount;
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;

/// Tracks rolling win rate and payoff ratio for Kelly criterion sizing.
struct KellyEstimator {
    wins:             u32,
    losses:           u32,
    total_win:        f64,
    total_loss:       f64,
    min_size:         f64,
    max_size:         f64,
    bankroll_pct_cap: f64,
}

impl KellyEstimator {
    fn new(min_size: f64, max_size: f64) -> Self {
        Self {
            wins: 0, losses: 0, total_win: 0.0, total_loss: 0.0,
            min_size, max_size, bankroll_pct_cap: 0.20,
        }
    }

    fn record(&mut self, pnl: f64) {
        if pnl > 0.0 {
            self.wins      += 1;
            self.total_win += pnl;
        } else {
            self.losses    += 1;
            self.total_loss += pnl.abs();
        }
    }

    /// Falls back to `min_size` until we have ≥ 10 trades.
    fn size(&self, bankroll: f64) -> f64 {
        let n = self.wins + self.losses;
        if n < 10 || self.total_loss == 0.0 {
            return self.min_size;
        }
        let p = self.wins as f64 / n as f64;
        let q = 1.0 - p;
        let avg_win  = self.total_win  / self.wins  as f64;
        let avg_loss = self.total_loss / self.losses as f64;
        let b = avg_win / avg_loss;
        let kelly_f = (p * b - q) / b;
        if kelly_f <= 0.0 { return self.min_size; }
        (kelly_f * bankroll).min(bankroll * self.bankroll_pct_cap)
            .clamp(self.min_size, self.max_size)
    }
}

struct SnapshotMeta<'a> {
    score:          decision::ScoreInfo,
    reject_reason:  Option<&'a str>,
    kelly_size:     f64,
    operation_mode: &'static str,
    fair_value:     Option<f64>,
}

/// Write the live market snapshot to logs/snapshot.json so the dashboard can read it.
fn write_snapshot_json(snap: &MarketSnapshot, meta: &SnapshotMeta<'_>) {
    let ts        = Utc::now().to_rfc3339();
    let remaining = snap.remaining_secs().max(0.0);
    let elapsed   = snap.elapsed_secs().max(0.0)   as i64;
    let diff      = snap.reference_price - snap.strike_price;
    let snipe_active = remaining >= 10.0 && remaining <= 30.0;
    let extreme_vol = if snap.strike_price > 0.0 {
        ((snap.reference_price - snap.strike_price) / snap.strike_price * 100.0).abs() > 5.0
    } else {
        false
    };
    let json = serde_json::json!({
        "ts":            ts,
        "market":        snap.market_id,
        "reference":     snap.reference_price,
        "strike":        snap.strike_price,
        "diff":          diff,
        "momentum":      snap.momentum_usd_per_sec,
        "yes":           snap.yes_price,
        "no":            snap.no_price,
        "remaining":     remaining as i64,
        "elapsed":       elapsed,
        "liquidity_yes": snap.liquidity_yes,
        "liquidity_no":  snap.liquidity_no,
        "volume_5m":     snap.volume_5m,
        "spread":        snap.spread,
        "oi_5m_pct":     snap.oi_5m_pct,
        "score":         meta.score.total,
        "confidence":    meta.score.confidence,
        "score_breakdown": {
            "window_delta": meta.score.window_delta,
            "momentum":     meta.score.momentum,
            "rsi":          meta.score.rsi,
            "volume":       meta.score.volume,
            "arb_edge":     meta.score.arb_edge,
        },
        "reject_reason":  meta.reject_reason,
        "fair_value":     meta.fair_value,
        "kelly_size":     meta.kelly_size,
        "operation_mode": meta.operation_mode,
        "snipe_active":   snipe_active,
        "extreme_vol":    extreme_vol,
    });
    if let Err(e) = std::fs::write("logs/snapshot.json", json.to_string()) {
        tracing::warn!("main: no se puede escribir snapshot.json: {e}");
    }
}

/// Write the active sim position to logs/sim_active.json.
fn write_active_json(
    active: bool,
    snap: &MarketSnapshot,
    pos: Option<(&Position, f64)>,  // (position, peak_pnl_pct)
    current_price: f64,
) {
    let json = if active {
        if let Some((p, _peak)) = pos {
            let pnl_pct = p.pnl_pct(current_price);
            let pnl_usd = pnl_pct * p.size_usdc;
            serde_json::json!({
                "active":       true,
                "market":       p.market_id,
                "side":         format!("{:?}", p.side),
                "entry_price":  p.entry_price,
                "size_usdc":    p.size_usdc,
                "size_tokens":  p.size_usdc / p.entry_price,
                "opened_at_ns": p.entry_time_ns,
                "current_price": current_price,
                "pnl_pct":      pnl_pct,
                "pnl_usd":      pnl_usd,
                "remaining_secs": snap.remaining_secs().max(0.0),
                "strike":       snap.strike_price,
                "reference":    snap.reference_price,
                "diff":         snap.reference_price - snap.strike_price,
            })
        } else {
            serde_json::json!({ "active": false })
        }
    } else {
        serde_json::json!({ "active": false })
    };
    if let Err(e) = std::fs::write("logs/sim_active.json", json.to_string()) {
        tracing::warn!("main: no se puede escribir sim_active.json: {e}");
    }
}

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider().install_default().expect("rustls init");
    tracing_subscriber::fmt::init();

    let cfg  = config::load_config(Path::new("config/markets.toml"));
    let mode = cfg.mode.clone();
    let starting_balance = cfg.simulated_balance_usdc;
    let rcfg: RiskConfig = cfg.risk.clone();

    tracing::info!("btcbot starting | mode={mode:?} | {} market(s) configured",
        cfg.markets.len());

    let btc_cfg = config::find_market(&cfg.markets, btcbot_core::BTC_5M_UPDOWN);
    let dcfg    = decision::decision_config_for(btc_cfg);

    let mut sim_account = SimAccount::new(starting_balance);
    let mut kelly = KellyEstimator::new(100.0, 200.0);

    let (tx, mut rx) = mpsc::channel::<MarketSnapshot>(64);

    for market_cfg in &cfg.markets {
        tracing::info!("feed: registering market '{}'", market_cfg.id);
        feed::spawn_for_market(market_cfg, tx.clone());
    }
    drop(tx);

    struct PositionState {
        position:     Position,
        peak_pnl_pct: f64,
        neg_risk:     bool,
        token_id:     String,
    }
    let mut positions: HashMap<String, PositionState> = HashMap::new();
    // Dedup rejection reason logging: only log on first occurrence or when it changes.
    let mut last_reject: HashMap<String, String> = HashMap::new();

    while let Some(snap) = rx.recv().await {
        let score     = decision::score_info(&snap);
        let fair_val  = decision::fair_value_up(&snap, dcfg.vol_usd_per_sqrts);
        let kelly_sz  = kelly.size(sim_account.balance());
        let last_rej  = last_reject.get(&snap.market_id).map(|s| s.as_str());
        let meta = SnapshotMeta {
            score,
            reject_reason:  last_rej,
            kelly_size:     kelly_sz,
            operation_mode: dcfg.operation_mode.as_str(),
            fair_value:     fair_val,
        };
        write_snapshot_json(&snap, &meta);

        let mid = snap.market_id.clone();
        match positions.remove(&mid) {
            None => {
                match decision::evaluate(&snap, &dcfg) {
                    Evaluation::Enter(sig) => {
                        let size = kelly.size(sim_account.balance());
                        let token_id = match sig.direction {
                            Direction::Up   => snap.token_id_yes.clone(),
                            Direction::Down => snap.token_id_no.clone(),
                        };
                        let neg_risk = snap.neg_risk;
                        match execution::place_entry_order(
                            &mode, &token_id, neg_risk, sig.side, size, sig.token_price,
                        ).await {
                            Ok(fill) => {
                                tracing::info!(
                                    "[ENTRY|{:?}] market={mid} dir={:?} side={:?} \
                                     fill={:.4} size={:.2}$ conf={:.2}",
                                    mode, sig.direction, sig.side,
                                    fill.token_price, fill.size_usdc, sig.confidence,
                                );
                                sim_account.try_open(&mid, fill.size_usdc);
                                last_reject.remove(&mid);
                                let pos_state = PositionState {
                                    position: Position {
                                        market_id:     snap.market_id.clone(),
                                        side:          sig.side,
                                        entry_price:   fill.token_price,
                                        size_usdc:     fill.size_usdc,
                                        entry_time_ns: snap.timestamp_ns,
                                    },
                                    peak_pnl_pct: 0.0,
                                    neg_risk,
                                    token_id,
                                };
                                let cur = match sig.side {
                                    Side::Yes => snap.yes_price,
                                    Side::No  => snap.no_price,
                                };
                                write_active_json(true, &snap, Some((&pos_state.position, 0.0)), cur);
                                positions.insert(mid, pos_state);
                            }
                            Err(e) => tracing::warn!("entry rejected on {mid}: {e}"),
                        }
                    }
                    Evaluation::Reject(reason) => {
                        let r = reason.as_str().to_string();
                        if last_reject.get(&mid) != Some(&r) {
                            tracing::debug!("decision: reject={r}");
                            last_reject.insert(mid.clone(), r);
                        }
                        write_active_json(false, &snap, None, 0.0);
                    }
                }
            }
            Some(mut state) => {
                let cur = match state.position.side {
                    Side::Yes => snap.yes_price,
                    Side::No  => snap.no_price,
                };
                let pnl = state.position.pnl_pct(cur);
                if pnl > state.peak_pnl_pct { state.peak_pnl_pct = pnl; }
                let underlying_diff = snap.reference_price - snap.strike_price;

                write_active_json(true, &snap, Some((&state.position, state.peak_pnl_pct)), cur);

                match risk::should_close(
                    &state.position,
                    cur,
                    snap.remaining_secs(),
                    underlying_diff,
                    state.peak_pnl_pct,
                    &rcfg,
                ) {
                    CloseReason::Hold => { positions.insert(mid, state); }
                    reason => {
                        let size_tokens = state.position.size_usdc / state.position.entry_price;
                        match execution::place_close_order(
                            &mode, &state.token_id, state.neg_risk,
                            state.position.side, size_tokens, cur,
                        ).await {
                            Ok(fill) => {
                                let pnl_usd = fill.size_usdc - state.position.size_usdc;
                                tracing::info!(
                                    "[CLOSE|{:?}] market={mid} reason={} \
                                     fill={:.4} pnl={:+.1}% pnl_usd={:+.2}$",
                                    mode, reason.as_str(), fill.token_price, pnl * 100.0, pnl_usd,
                                );
                                kelly.record(pnl_usd);
                                sim_account.record_close(
                                    &mid,
                                    state.position.side,
                                    state.position.entry_price,
                                    fill.token_price,
                                    state.position.size_usdc,
                                    reason.as_str(),
                                );
                                write_active_json(false, &snap, None, 0.0);
                            }
                            Err(e) => {
                                tracing::error!(
                                    "close failed on {mid} ({reason:?}): {e} — will retry next tick"
                                );
                                positions.insert(mid, state);
                            }
                        }
                    }
                }
            }
        }
    }

    tracing::info!(
        "btcbot shutdown | balance={:.2} realized_pnl={:+.2} wins={} losses={}",
        sim_account.balance(),
        sim_account.realized_pnl(),
        sim_account.wins(),
        sim_account.losses(),
    );
}
