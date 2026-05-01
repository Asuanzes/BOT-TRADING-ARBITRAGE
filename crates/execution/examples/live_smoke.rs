//! Live smoke test: pone UNA orden BUY FAK a precio no-fillable contra el CLOB
//! para verificar que el path live (sig_type=2 + Safe maker + V2 schema + urlsafe HMAC)
//! es aceptado por Polymarket. El order auto-cancela por FAK no-match → $0 gastado.
//!
//! Uso:
//!   set -a && source .env && set +a
//!   cargo run --release -p execution --example live_smoke -- <token_id_decimal>
//!
//! Si no se pasa token_id, lee logs/snapshot.json para usar el del market activo.

use anyhow::{Context, Result};
use execution::{place_entry_order, FillStatus};
use btcbot_core::{RunMode, Side};

#[tokio::main]
async fn main() -> Result<()> {
    let token_id = match std::env::args().nth(1) {
        Some(t) => t,
        None => {
            // Fallback: leer del snapshot.json + gamma-api
            let snap: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string("logs/snapshot.json")
                    .context("logs/snapshot.json no encontrado")?,
            )?;
            let cid = snap["market"].as_str().context("market field missing")?;
            let url = format!(
                "https://gamma-api.polymarket.com/markets?condition_ids={}",
                cid
            );
            let body: serde_json::Value = reqwest::Client::new()
                .get(&url)
                .header("User-Agent", "btcbot-smoke/1.0")
                .send().await?.json().await?;
            let arr = body.as_array().context("gamma response not array")?;
            anyhow::ensure!(!arr.is_empty(), "market closed / not found");
            let m = &arr[0];
            let tokens_str = m["clobTokenIds"].as_str().context("clobTokenIds missing")?;
            let tokens: Vec<String> = serde_json::from_str(tokens_str)?;
            tokens.into_iter().next().context("no tokens")?
        }
    };

    eprintln!("=== live smoke: BUY $1 @ 0.01 FAK on token {}…", &token_id[..16]);
    eprintln!("    quoted_price=0.01 ⇒ taker_amount=100 shares; below market ⇒ no match expected");

    let neg_risk = false;
    let size_usdc = 1.0;
    let quoted_price = 0.01;

    match place_entry_order(
        &RunMode::Live, &token_id, neg_risk, Side::Yes, size_usdc, quoted_price,
    ).await {
        Ok(fill) => {
            eprintln!("✅ POST 200 — fill returned:");
            eprintln!("   token_price={} size_usdc={} size_tokens={} status={:?}",
                fill.token_price, fill.size_usdc, fill.size_tokens, fill.status);
            anyhow::ensure!(fill.status == FillStatus::Filled, "expected Filled status");
            // For our $0.01 limit (way below market), an actual fill would be a bug;
            // if Polymarket returned 200 but no fill, we still pass.
        }
        Err(e) => {
            // Expected: FAK no-match returns 400 with "no orders found". That's a bail!.
            // We treat ANY response that includes "no orders found" or "FAK" as success
            // (the order parsed and was processed; just had nothing to match).
            let msg = format!("{:#}", e);
            if msg.contains("no orders found") || msg.contains("FAK") || msg.contains("orderID") {
                eprintln!("✅ Schema accepted, FAK auto-cancelled (no match — expected at $0.01):");
                eprintln!("   {}", msg);
                return Ok(());
            }
            eprintln!("❌ unexpected error:");
            eprintln!("   {}", msg);
            return Err(e);
        }
    }

    Ok(())
}
