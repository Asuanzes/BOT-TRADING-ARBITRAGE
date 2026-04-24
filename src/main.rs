use btcbot_core::{DecisionConfig, MarketSnapshot, Position, Side};
use risk::{CloseReason, RiskConfig};
use std::collections::HashMap;
use tokio::sync::mpsc;

const SIZE_USDC: f64 = 10.0;

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider().install_default().expect("rustls init");
    tracing_subscriber::fmt::init();

    let dcfg = DecisionConfig::default();
    let rcfg = RiskConfig::default();

    let (tx, mut rx) = mpsc::channel::<MarketSnapshot>(64);
    feed::spawn(tx);

    let mut positions: HashMap<String, Position> = HashMap::new();

    while let Some(snap) = rx.recv().await {
        let mid = snap.market_id.clone();
        match positions.remove(&mid) {
            None => {
                if let Some(sig) = decision::evaluate(&snap, &dcfg) {
                    tracing::info!(
                        "[ENTRY] market={mid} direction={:?} confidence={:.2} \
                         token_price={:.4} elapsed={:.0}s remaining={:.0}s",
                        sig.direction, sig.confidence, sig.token_price,
                        snap.elapsed_secs(), snap.remaining_secs()
                    );
                    positions.insert(mid, Position {
                        market_id:     snap.market_id.clone(),
                        side:          sig.side,
                        entry_price:   sig.token_price,
                        size_usdc:     SIZE_USDC,
                        entry_time_ns: snap.timestamp_ns,
                    });
                }
            }
            Some(pos) => {
                let cur = match pos.side { Side::Yes => snap.yes_price, Side::No => snap.no_price };
                match risk::should_close(&pos, cur, snap.remaining_secs(), &rcfg) {
                    CloseReason::Hold => { positions.insert(mid, pos); }
                    reason => tracing::info!(
                        "[CLOSE] market={mid} reason={reason:?} pnl={:+.1}% elapsed={:.0}s",
                        pos.pnl_pct(cur) * 100.0, snap.elapsed_secs()
                    ),
                }
            }
        }
    }
}
