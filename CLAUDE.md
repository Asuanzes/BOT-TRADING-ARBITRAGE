# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Rust bot
cargo build --release                  # build binary → target/release/btcbot
cargo test                             # all workspace tests
cargo test -p btcbot-decision          # single crate (replace with any crate name)
./target/release/btcbot                # run bot (reads config/markets.toml)
systemctl restart btcbot               # restart systemd service
journalctl -u btcbot -f                # stream service logs

# Solidity contracts
cd contracts && forge build
cd contracts && forge test

# Data analysis
python3 scripts/analyze_features.py   # win-rate / feature correlation from data/raw/
```

## Architecture

Seven-crate Rust workspace. The main bot pipeline is: **Feed → Decision → Risk → Execution**.

### Crates

| Crate | Role |
|-------|------|
| `crates/core` | Shared domain types: `RunMode`, `MarketConfig`, `MarketSnapshot`, `Direction`, `Side`, `Signal`, `Position`, `DecisionConfig` |
| `crates/feed` | Polls Polymarket CLOB + Binance every 250 ms per market; emits `MarketSnapshot` over mpsc channel. Optional Chainlink Data Streams for authoritative strike price; falls back to Binance if credentials absent. |
| `crates/decision` | `evaluate(snapshot, config) → Enter(Signal) \| Reject(RejectReason)`. Sequential filter chain: timing gates → volume/spread → price-diff or momentum fast-path → liquidity → momentum veto → token price. |
| `crates/risk` | `should_close(position, snapshot, …) → CloseReason`. Checks: timeout, reversal, liquidity drain, spread blowout, take-profit, trailing stop, stop-loss. Stop-loss is suppressed when the underlying still strongly favors the position. When `|diff| > let_run_diff_usd`, fixed TP is skipped ("let winners run"). |
| `crates/execution` | `place_entry_order` / `place_close_order`. Simulation: instant fill at quoted price. Live: EIP-712 signed order → POST to Polymarket CLOB. Domain separator differs for CTF vs Neg-Risk CTF markets. |
| `crates/chainlink` | Chainlink Data Streams v3 BTC/USD client (HMAC-SHA256 auth). Decodes int192 price with 18 decimals. Used by `crates/feed` only. |
| `crates/ingest` | Standalone binary for raw data collection (Deribit, Binance, Coinalyze → gzip ndjson in `data/raw/`). Not part of the live bot loop. |

### Orchestration (`src/`)

- `src/main.rs` — `tokio::select!` loop: receives `MarketSnapshot` from feed tasks, dispatches to `decision::evaluate` (no open position) or `risk::should_close` (position held), calls execution, tracks `HashMap<market_id, PositionState>`. Emits simulation summary on shutdown.
- `src/config.rs` — loads `config/markets.toml`, validates fields, builds `DecisionConfig` per market.
- `src/sim.rs` — `SimAccount`: cash / realized PnL bookkeeping; writes `logs/equity.csv` (per-trade) and `logs/stats.json` (aggregate). Dashboard polls these files.

### Data flow

```
Feed tasks (one per market, 250 ms)
    │  MarketSnapshot via mpsc
    ▼
Orchestrator (src/main.rs)
    ├─ no position → decision::evaluate → Enter → execution::place_entry_order → SimAccount
    └─ has position → risk::should_close → close → execution::place_close_order → SimAccount
```

### Key non-obvious patterns

- **Rejection dedup**: `RejectReason` only logs on first occurrence or when it changes; same reason repeating is silent.
- **Binary market complementarity**: when holding YES, the bot checks NO liquidity as a proxy for YES exit demand (and vice versa).
- **Momentum fast-path**: when `|diff|` is in the "strong" band and momentum amplifies the direction, a lower `min_price_diff_usd_strong` threshold applies.
- **Window detection**: window start is derived from the market slug timestamp, not from Polymarket's `startDate` (which is the market creation time).

## Configuration

`config/markets.toml` — one `[[markets]]` block per market. Top-level keys: `mode` (`"simulation"` | `"live"`), `simulated_balance_usdc`, `entry_size_usdc`.

**Required env vars for live mode:**
```
POLY_API_KEY, POLY_API_SECRET, POLY_PASSPHRASE, POLY_PRIVATE_KEY, POLY_ADDRESS
```
**Optional (Chainlink):**
```
CHAINLINK_API_KEY, CHAINLINK_API_SECRET, CHAINLINK_BTC_USD_FEED_ID
```

## Contracts (Foundry)

`contracts/src/PredictionMarket.sol` — virtual UP/DOWN BTC prediction market; operator-controlled rounds; designed for Chainlink Automation integration. Target chain: Arbitrum Sepolia. Requires `ARBITRUM_SEPOLIA_RPC_URL` and `ARBISCAN_API_KEY` in `contracts/.env`.

## Claude Code rules

- **Shell first**: use `ls`, `rg`, `grep`, `cargo` to orient before opening files.
- **File size**: don't open files >~500 lines without being asked.
- **Focus**: prioritize `src/` and the specific crate under discussion.
- **Credentials**: never print or echo values from `.env` or `config/`; treat them as opaque env vars.
- **Incremental changes**: propose a plan first; apply the minimal diff; don't rewrite whole files.
