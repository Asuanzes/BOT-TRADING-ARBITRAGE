mod config;

use btcbot_core::{MarketSnapshot, Position, Side};
use risk::{CloseReason, RiskConfig};
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;

const SIZE_USDC: f64 = 10.0;

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
                    // Entry goes through the execution layer. In Simulation the fill is
                    // synthetic (at sig.token_price); in Live this will hit the CLOB.
                    match execution::place_entry_order(
                        &mode, &mid, sig.side, SIZE_USDC, sig.token_price,
                    ).await {
                        Ok(fill) => {
                            tracing::info!(
                                "[ENTRY|{:?}] market={mid} dir={:?} side={:?} \
                                 fill={:.4} size={:.2}$ conf={:.2}",
                                mode, sig.direction, sig.side,
                                fill.token_price, fill.size_usdc, sig.confidence,
                            );
                            positions.insert(mid, PositionState {
                                position: Position {
                                    market_id:     snap.market_id.clone(),
                                    side:          sig.side,
                                    // Use the actual fill price, not the quoted one —
                                    // slippage lives here when the live path is wired.
                                    entry_price:   fill.token_price,
                                    size_usdc:     fill.size_usdc,
                                    entry_time_ns: snap.timestamp_ns,
                                },
                                peak_pnl_pct: 0.0,
                            });
                        }
                        Err(e) => tracing::warn!("entry rejected on {mid}: {e}"),
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
                        // Token amount to unwind = size_usdc / entry_price.
                        // (Holds as long as size_usdc reflects the notional actually filled.)
                        let size_tokens = state.position.size_usdc / state.position.entry_price;
                        match execution::place_close_order(
                            &mode, &mid, state.position.side, size_tokens, cur,
                        ).await {
                            Ok(fill) => tracing::info!(
                                "[CLOSE|{:?}] market={mid} reason={reason:?} \
                                 fill={:.4} pnl={:+.1}%",
                                mode, fill.token_price, pnl * 100.0,
                            ),
                            Err(e) => {
                                // Keep tracking the position so the next snapshot retries.
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
}
