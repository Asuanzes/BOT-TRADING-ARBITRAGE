mod config;

use btcbot_core::{MarketSnapshot, Position, RunMode, Side, Signal};
use risk::{CloseReason, RiskConfig};
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;

const SIZE_USDC: f64 = 10.0;

/// Entry-order dispatch — the only function that needs to change when moving to live.
/// Simulation: logs the signal; no exchange call is made.
/// Live: TODO — call execution::place_entry_order(market_id, sig, size_usdc).await
fn execute_entry(mode: &RunMode, market_id: &str, sig: &Signal, size_usdc: f64) {
    match mode {
        RunMode::Simulation => tracing::info!(
            "[SIM|ENTRY] market={market_id} direction={:?} side={:?} \
             price={:.4} size={size_usdc:.0}$ confidence={:.2}",
            sig.direction, sig.side, sig.token_price, sig.confidence,
        ),
        RunMode::Live => {
            tracing::warn!("[LIVE|ENTRY] execution not yet implemented — no order sent");
        }
    }
}

/// Close-order dispatch — mirrors execute_entry for the exit side.
/// Live: TODO — call execution::place_close_order(market_id).await
fn execute_close(mode: &RunMode, market_id: &str, reason: CloseReason, pnl_pct: f64) {
    match mode {
        RunMode::Simulation => tracing::info!(
            "[SIM|CLOSE] market={market_id} reason={reason:?} pnl={pnl_pct:+.1}%",
        ),
        RunMode::Live => {
            tracing::warn!("[LIVE|CLOSE] execution not yet implemented — position not closed on exchange");
        }
    }
}

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider().install_default().expect("rustls init");
    tracing_subscriber::fmt::init();

    let cfg  = config::load_config(Path::new("config/markets.toml"));
    let mode = cfg.mode.clone();

    tracing::info!("btcbot starting | mode={mode:?} | {} market(s) configured",
        cfg.markets.len());

    // Build a DecisionConfig for every configured market up front.
    // decision_config_for() is market-agnostic — it reads thresholds from the MarketConfig entry.
    // When a second market becomes active, add its dcfg lookup inside the snapshot loop below.
    //
    // NOTE: snapshot.market_id today is the Polymarket condition ID (a hash), not the logical name.
    // For generic per-market lookup in the loop, the feed will need to also embed the logical id.
    let btc_cfg = config::find_market(&cfg.markets, btcbot_core::BTC_5M_UPDOWN);
    let dcfg    = decision::decision_config_for(btc_cfg);
    let rcfg    = RiskConfig::default();

    let (tx, mut rx) = mpsc::channel::<MarketSnapshot>(64);

    // Spawn one feed task per configured market.
    // feed::spawn_for_market warns and skips any market without a feed implementation.
    // To activate ETH_5M_UPDOWN: add its entry to config/markets.toml and extend
    // feed::spawn_for_market with an ETH feed loop.
    for market_cfg in &cfg.markets {
        tracing::info!("feed: registering market '{}'", market_cfg.id);
        feed::spawn_for_market(market_cfg, tx.clone());
    }
    drop(tx); // close sender side; channel ends when all feed tasks exit

    // The positions map and the snapshot loop below are already market-agnostic:
    // they key everything on snapshot.market_id and handle N concurrent open positions.
    // peak_pnl_pct is a high-water mark tracked locally (not persisted on Position)
    // so the shared core type stays a pure position record.
    struct PositionState {
        position:     Position,
        peak_pnl_pct: f64,
    }
    let mut positions: HashMap<String, PositionState> = HashMap::new();

    while let Some(snap) = rx.recv().await {
        let mid = snap.market_id.clone();
        match positions.remove(&mid) {
            None => {
                if let Some(sig) = decision::evaluate(&snap, &dcfg) {
                    execute_entry(&mode, &mid, &sig, SIZE_USDC);
                    // Position is tracked locally in both modes for P&L accounting.
                    positions.insert(mid, PositionState {
                        position: Position {
                            market_id:     snap.market_id.clone(),
                            side:          sig.side,
                            entry_price:   sig.token_price,
                            size_usdc:     SIZE_USDC,
                            entry_time_ns: snap.timestamp_ns,
                        },
                        peak_pnl_pct: 0.0,
                    });
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

                match risk::should_close(
                    &state.position,
                    cur,
                    snap.remaining_secs(),
                    underlying_diff,
                    state.peak_pnl_pct,
                    &rcfg,
                ) {
                    CloseReason::Hold => { positions.insert(mid, state); }
                    reason => execute_close(&mode, &mid, reason, pnl * 100.0),
                }
            }
        }
    }
}
