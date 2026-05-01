//! Execution layer: the only place that talks to the exchange.
//!
//! Simulation mode: orders synthesized as immediate fills at the quoted price.
//! Live mode: EIP-712 signed limit orders submitted to the Polymarket CLOB.
//!   Auth: HMAC-SHA256 over (timestamp + method + path + body) with the API secret.
//!   Signing: secp256k1 over the EIP-712 digest of the Order struct.
//!   Exchange contract depends on the market's `neg_risk` flag (CTF vs Neg Risk CTF).

use anyhow::{bail, Context, Result};
use btcbot_core::{RunMode, Side};
use std::sync::OnceLock;

/// Shared HTTP client — created once, reuses TCP connections across all CLOB calls.
/// Reduces latency by eliminating TCP handshake + TLS negotiation on each order.
static CLOB_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn clob_client() -> &'static reqwest::Client {
    CLOB_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .connection_verbose(false)
            .build()
            .expect("reqwest client init failed")
    })
}

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Fill {
    pub token_price: f64,
    pub size_usdc:   f64,
    pub size_tokens: f64,
    pub status:      FillStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillStatus {
    Simulated,
    Filled,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Submit a BUY to open a position on the given token.
/// `token_id`: Polymarket CLOB token ID (decimal uint256 string).
/// `neg_risk`: true → use Neg Risk CTF Exchange, false → use CTF Exchange.
pub async fn place_entry_order(
    mode:         &RunMode,
    token_id:     &str,
    neg_risk:     bool,
    _token_side:  Side,
    size_usdc:    f64,
    quoted_price: f64,
) -> Result<Fill> {
    match mode {
        RunMode::Simulation => Ok(simulated_fill(size_usdc, quoted_price)),
        RunMode::Live => {
            let creds = PolyCredentials::from_env()
                .context("POLY_* env vars missing — cannot place live order")?;
            poly_buy(&creds, token_id, neg_risk, size_usdc, quoted_price).await
        }
    }
}

/// Submit a SELL to close an existing position on the given token.
pub async fn place_close_order(
    mode:         &RunMode,
    token_id:     &str,
    neg_risk:     bool,
    _token_side:  Side,
    size_tokens:  f64,
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
            let creds = PolyCredentials::from_env()
                .context("POLY_* env vars missing — cannot place live close")?;
            poly_sell(&creds, token_id, neg_risk, size_tokens, quoted_price).await
        }
    }
}

// ── Simulation ────────────────────────────────────────────────────────────────

fn simulated_fill(size_usdc: f64, quoted_price: f64) -> Fill {
    let tokens = if quoted_price > 0.0 { size_usdc / quoted_price } else { 0.0 };
    Fill {
        token_price: quoted_price,
        size_usdc,
        size_tokens: tokens,
        status: FillStatus::Simulated,
    }
}

// ── Polymarket credentials ────────────────────────────────────────────────────

const CLOB: &str = "https://clob.polymarket.com";

// Polygon mainnet exchange contracts — V2 (verified on-chain 2026-04-30 vía eip712Domain())
const CTF_EXCHANGE:          &str = "E111180000d2663C0091e4f400237545B87B996B";
const NEG_RISK_CTF_EXCHANGE: &str = "d91E80cF2E7be2e162c6513ceD06f1dD0dA35296";

struct PolyCredentials {
    api_key:     String,
    api_secret:  Vec<u8>,             // base64-decoded HMAC key (urlsafe)
    passphrase:  String,
    signing_key: k256::ecdsa::SigningKey,
    address:     [u8; 20],            // EOA address (signer)
    funder:      [u8; 20],            // maker = Safe if POLY_SAFE_ADDRESS set, else EOA
    sig_type:    u8,                  // 2 if Safe, 0 if pure EOA
}

impl PolyCredentials {
    fn from_env() -> Result<Self> {
        let api_key    = std::env::var("POLY_API_KEY").context("POLY_API_KEY not set")?;
        let secret_b64 = std::env::var("POLY_API_SECRET").context("POLY_API_SECRET not set")?;
        let passphrase = std::env::var("POLY_PASSPHRASE").context("POLY_PASSPHRASE not set")?;
        let priv_hex   = std::env::var("POLY_PRIVATE_KEY").context("POLY_PRIVATE_KEY not set")?;

        use base64::Engine as _;
        let api_secret = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(secret_b64.trim().trim_end_matches('='))
            .context("POLY_API_SECRET is not valid base64 (URL-safe)")?;

        let priv_bytes = hex::decode(priv_hex.trim().trim_start_matches("0x"))
            .context("POLY_PRIVATE_KEY is not valid hex")?;
        let signing_key = k256::ecdsa::SigningKey::from_slice(&priv_bytes)
            .context("POLY_PRIVATE_KEY is not a valid secp256k1 scalar")?;

        // Derive Ethereum address: keccak256(uncompressed_pubkey[1..])[12..]
        let pub_encoded = signing_key.verifying_key().to_encoded_point(false);
        let pub_bytes   = pub_encoded.as_bytes(); // 65 bytes: 04 || x || y
        let hash        = keccak256(&pub_bytes[1..]);
        let mut address = [0u8; 20];
        address.copy_from_slice(&hash[12..]);

        // POLY_SAFE_ADDRESS is the funder when set (Polymarket Safe / proxy holding USDC).
        // sig_type 2 = POLY_GNOSIS_SAFE (EIP-1271 via the Safe owner). 0 = plain EOA.
        let (funder, sig_type) = match std::env::var("POLY_SAFE_ADDRESS") {
            Ok(s) if !s.trim().is_empty() => {
                let bytes = hex::decode(s.trim().trim_start_matches("0x"))
                    .context("POLY_SAFE_ADDRESS is not valid hex")?;
                if bytes.len() != 20 {
                    bail!("POLY_SAFE_ADDRESS must be 20 bytes (40 hex chars)");
                }
                let mut a = [0u8; 20];
                a.copy_from_slice(&bytes);
                (a, 2u8)
            }
            _ => (address, 0u8),
        };

        Ok(Self { api_key, api_secret, passphrase, signing_key, address, funder, sig_type })
    }

    fn address_hex(&self) -> String {
        format!("0x{}", hex::encode(self.address))
    }

    fn funder_hex(&self) -> String {
        format!("0x{}", hex::encode(self.funder))
    }
}

// ── Live order placement ──────────────────────────────────────────────────────

// Polymarket V2 amount precision (verified live 2026-04-30):
//   BUY:  maker=USDC → 2 decimals,  taker=shares → 4 decimals
//   SELL: maker=shares → 2 decimals, taker=USDC  → 4 decimals
// i.e. *maker* is always the coarser side (2 dec), *taker* the finer (4 dec),
// regardless of which is USDC and which is shares.
// In micro-units (×1_000_000): 2 dec → step 10_000, 4 dec → step 100.
const STEP_MAKER_MICRO: u64 = 10_000;  // 2 decimals (0.01)
const STEP_TAKER_MICRO: u64 =    100;  // 4 decimals (0.0001)
const TICK_SIZE:         f64 = 0.01;    // Polymarket BTC 5m markets
const SLIPPAGE_TICKS:    i32 = 2;       // pad 2 ticks: gana fill rate vs 1 tick (FAK no-match repetido)

fn round_micro(value: f64, step: u64) -> u64 {
    let raw = (value * 1_000_000.0).floor() as u64;
    raw - (raw % step)
}

fn snap_up(p: f64)   -> f64 { (p / TICK_SIZE).ceil()  * TICK_SIZE }
fn snap_down(p: f64) -> f64 { (p / TICK_SIZE).floor() * TICK_SIZE }

async fn poly_buy(
    creds:        &PolyCredentials,
    token_id:     &str,
    neg_risk:     bool,
    size_usdc:    f64,
    quoted_price: f64,
) -> Result<Fill> {
    // BUY: limit = snap_up(quoted) + 1 tick. Order says "I'll pay at most this per share".
    // If the actual ask is below limit at fill time, we get a better price.
    let limit_price = (snap_up(quoted_price) + SLIPPAGE_TICKS as f64 * TICK_SIZE).clamp(0.01, 0.99);
    let maker_amount = round_micro(size_usdc, STEP_MAKER_MICRO);                    // USDC, 2 dec
    let taker_amount = round_micro(size_usdc / limit_price, STEP_TAKER_MICRO);     // shares, 4 dec
    if taker_amount == 0 {
        bail!("computed taker_amount == 0 (size_usdc={size_usdc}, limit={limit_price})");
    }

    tracing::info!(
        "poly_buy: size=${:.2} quoted={:.4} limit={:.4} maker={} taker={}",
        size_usdc, quoted_price, limit_price, maker_amount, taker_amount,
    );

    let signed = sign_order(creds, token_id, neg_risk, 0 /* BUY */, maker_amount, taker_amount)?;
    submit_order(creds, &signed, token_id, 0, maker_amount, taker_amount, size_usdc, quoted_price).await
}

async fn poly_sell(
    creds:        &PolyCredentials,
    token_id:     &str,
    neg_risk:     bool,
    size_tokens:  f64,
    quoted_price: f64,
) -> Result<Fill> {
    // SELL: limit = snap_down(quoted) - 1 tick. "I'll accept at least this per share".
    let limit_price = (snap_down(quoted_price) - SLIPPAGE_TICKS as f64 * TICK_SIZE).clamp(0.01, 0.99);
    let maker_amount = round_micro(size_tokens, STEP_MAKER_MICRO);                        // shares, 2 dec
    let taker_amount = round_micro(size_tokens * limit_price, STEP_TAKER_MICRO);          // USDC, 4 dec
    if maker_amount == 0 || taker_amount == 0 {
        bail!("computed amount == 0 (size_tokens={size_tokens}, limit={limit_price})");
    }
    let size_usdc = size_tokens * limit_price;

    tracing::info!(
        "poly_sell: tokens={:.4} quoted={:.4} limit={:.4} maker={} taker={}",
        size_tokens, quoted_price, limit_price, maker_amount, taker_amount,
    );

    let signed = sign_order(creds, token_id, neg_risk, 1 /* SELL */, maker_amount, taker_amount)?;
    submit_order(creds, &signed, token_id, 1, maker_amount, taker_amount, size_usdc, quoted_price).await
}

// ── EIP-712 order signing ─────────────────────────────────────────────────────

struct SignedOrder {
    salt:         u64,
    timestamp_ms: u64,
    signature:    String, // "0x" + 65 bytes hex
}

fn sign_order(
    creds:        &PolyCredentials,
    token_id:     &str,
    neg_risk:     bool,
    side:         u8,   // 0=BUY 1=SELL
    maker_amount: u64,
    taker_amount: u64,
) -> Result<SignedOrder> {
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let salt = timestamp_ms;

    // ORDER_TYPE_HASH — V2: removes taker/expiration/nonce/feeRateBps, adds timestamp/metadata/builder
    let type_hash = keccak256(
        b"Order(uint256 salt,address maker,address signer,uint256 tokenId,\
uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,\
uint256 timestamp,bytes32 metadata,bytes32 builder)"
    );

    let token_id_bytes = decimal_to_u256_be(token_id);

    // Encode struct: 12 × 32 bytes (V2). type_hash + 11 fields.
    // V2 schema verified byte-for-byte against a captured live Polymarket UI order
    // (logs/oracle_v2_verify.json). NO `taker`/`expiration`/`nonce`/`feeRateBps` here —
    // `expiration` is JSON-only, the others are gone in V2.
    let mut order_data = [0u8; 12 * 32];
    write_slot(&mut order_data, 0,  &type_hash);
    write_slot(&mut order_data, 1,  &u64_to_u256(salt));
    write_slot(&mut order_data, 2,  &addr_to_u256(&creds.funder));   // maker = Safe (or EOA)
    write_slot(&mut order_data, 3,  &addr_to_u256(&creds.address));  // signer = EOA
    write_slot(&mut order_data, 4,  &token_id_bytes);
    write_slot(&mut order_data, 5,  &u64_to_u256(maker_amount));
    write_slot(&mut order_data, 6,  &u64_to_u256(taker_amount));
    write_slot(&mut order_data, 7,  &u8_to_u256(side));
    write_slot(&mut order_data, 8,  &u8_to_u256(creds.sig_type));    // 2 = Safe, 0 = EOA
    write_slot(&mut order_data, 9,  &u64_to_u256(timestamp_ms));
    write_slot(&mut order_data, 10, &[0u8; 32]);                     // metadata = bytes32(0)
    write_slot(&mut order_data, 11, &[0u8; 32]);                     // builder  = bytes32(0)

    let struct_hash = keccak256(&order_data);
    let domain_sep  = domain_separator(neg_risk);

    // EIP-712 digest: 0x1901 || domain || struct
    let mut digest_input = [0u8; 66];
    digest_input[0] = 0x19;
    digest_input[1] = 0x01;
    digest_input[2..34].copy_from_slice(&domain_sep);
    digest_input[34..66].copy_from_slice(&struct_hash);
    let digest = keccak256(&digest_input);

    // Sign with secp256k1 (recoverable)
    let (sig, rec_id) = creds.signing_key
        .sign_prehash_recoverable(&digest)
        .context("secp256k1 signing failed")?;

    let sig_bytes = sig.to_bytes();
    let v         = u8::from(rec_id) + 27; // Ethereum v convention

    let mut full = [0u8; 65];
    full[..64].copy_from_slice(&sig_bytes);
    full[64] = v;

    Ok(SignedOrder {
        salt,
        timestamp_ms,
        signature: format!("0x{}", hex::encode(full)),
    })
}

fn domain_separator(neg_risk: bool) -> [u8; 32] {
    let exchange_hex = if neg_risk { NEG_RISK_CTF_EXCHANGE } else { CTF_EXCHANGE };
    let exchange_bytes = hex::decode(exchange_hex).expect("hardcoded exchange address");
    let mut exchange_addr = [0u8; 20];
    exchange_addr.copy_from_slice(&exchange_bytes);

    let domain_type_hash = keccak256(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
    );
    let name_hash = if neg_risk {
        keccak256(b"Polymarket Neg Risk CTF Exchange")
    } else {
        keccak256(b"Polymarket CTF Exchange")
    };
    let version_hash = keccak256(b"2");

    // 5 × 32 bytes
    let mut buf = [0u8; 5 * 32];
    write_slot(&mut buf, 0, &domain_type_hash);
    write_slot(&mut buf, 1, &name_hash);
    write_slot(&mut buf, 2, &version_hash);
    write_slot(&mut buf, 3, &u64_to_u256(137)); // Polygon mainnet chainId
    write_slot(&mut buf, 4, &addr_to_u256(&exchange_addr));

    keccak256(&buf)
}

// ── HTTP submission ───────────────────────────────────────────────────────────

async fn submit_order(
    creds:        &PolyCredentials,
    signed:       &SignedOrder,
    token_id:     &str,
    side:         u8,
    maker_amount: u64,
    taker_amount: u64,
    size_usdc:    f64,
    quoted_price: f64,
) -> Result<Fill> {
    let signer_addr = creds.address_hex();
    let funder_addr = creds.funder_hex();
    let side_str    = if side == 0 { "BUY" } else { "SELL" };

    let zeros32 = "0x0000000000000000000000000000000000000000000000000000000000000000";
    // Body shape verified against captured live Polymarket UI order. Notably:
    //   • outer `deferExec`+`postOnly` (NOT `waiting`)
    //   • inner `expiration:"0"` (JSON only, not in EIP-712)
    //   • `owner` is the api_key UUID, NOT the wallet
    //   • `orderType:"FAK"` matches our "instant fill at quote" semantics
    //   • salt as integer
    let body_json = serde_json::json!({
        "deferExec": false,
        "order": {
            "salt":          signed.salt,
            "maker":         &funder_addr,
            "signer":        &signer_addr,
            "tokenId":       token_id,
            "makerAmount":   maker_amount.to_string(),
            "takerAmount":   taker_amount.to_string(),
            "side":          side_str,
            "signatureType": creds.sig_type,
            "timestamp":     signed.timestamp_ms.to_string(),
            "expiration":    "0",
            "metadata":      zeros32,
            "builder":       zeros32,
            "signature":     &signed.signature,
        },
        "owner":     &creds.api_key,
        "orderType": "FAK",
        "postOnly":  false,
    });
    let body_str = body_json.to_string();

    // HMAC-SHA256 auth: sign timestamp + method + path + body
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let message   = format!("{}POST/order{}", timestamp, body_str);

    use hmac::Mac as _;
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&creds.api_secret)
        .context("invalid HMAC key")?;
    mac.update(message.as_bytes());
    use base64::Engine as _;
    // Polymarket expects URL-safe base64 for the HMAC signature. Standard b64 only
    // works when the digest happens to lack `+` and `/`; verified via captured headers.
    let sig_b64 = base64::engine::general_purpose::URL_SAFE.encode(mac.finalize().into_bytes());

    // POLY_ADDRESS header = signer wallet (NOT api_key — the comment that used to live
    // here was wrong; verified against py-clob-client headers and the captured order).
    let t0   = std::time::Instant::now();
    let resp = clob_client()
        .post(format!("{CLOB}/order"))
        .header("Content-Type", "application/json")
        .header("POLY_ADDRESS",    &signer_addr)
        .header("POLY_API_KEY",    &creds.api_key)
        .header("POLY_SIGNATURE",  sig_b64)
        .header("POLY_TIMESTAMP",  &timestamp)
        .header("POLY_PASSPHRASE", &creds.passphrase)
        .body(body_str)
        .send()
        .await
        .context("HTTP POST to Polymarket CLOB failed")?;

    let http_status = resp.status();
    let resp_body   = resp.text().await.unwrap_or_default();
    let rtt_ms = t0.elapsed().as_millis();
    tracing::debug!("clob: order RTT {}ms status={}", rtt_ms, http_status);

    if !http_status.is_success() {
        bail!("Polymarket CLOB {}: {}", http_status, resp_body);
    }

    let resp_json: serde_json::Value = serde_json::from_str(&resp_body)
        .context("failed to parse CLOB response")?;

    if resp_json.get("success").and_then(|v| v.as_bool()) == Some(false) {
        bail!("Polymarket order rejected: {}", resp_body);
    }

    // CLOB returns making/taking amounts (decimal strings) — derive the real fill
    // price from them. Fall back to quoted_price only if the response shape is unexpected.
    //   BUY  (side=0): we paid `making` USDC, received `taking` shares
    //   SELL (side=1): we sent `making` shares, received `taking` USDC
    let parse_amt = |k: &str| resp_json.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok());
    let making = parse_amt("makingAmount");
    let taking = parse_amt("takingAmount");
    let (fill_usdc, fill_tokens) = match (side, making, taking) {
        (0, Some(m), Some(t)) => (m, t),
        (1, Some(m), Some(t)) => (t, m),
        _                     => (size_usdc, if quoted_price > 0.0 { size_usdc / quoted_price } else { 0.0 }),
    };
    let fill_price = if fill_tokens > 0.0 { fill_usdc / fill_tokens } else { quoted_price };

    tracing::info!(
        "CLOB order accepted: orderID={} status={} fill_price={:.4} fill_usdc={:.4} fill_tokens={:.4}",
        resp_json.get("orderID").and_then(|v| v.as_str()).unwrap_or("?"),
        resp_json.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
        fill_price, fill_usdc, fill_tokens,
    );

    Ok(Fill {
        token_price: fill_price,
        size_usdc:   fill_usdc,
        size_tokens: fill_tokens,
        status: FillStatus::Filled,
    })
}

// ── ABI / crypto helpers ──────────────────────────────────────────────────────

fn keccak256(data: &[u8]) -> [u8; 32] {
    use sha3::Digest as _;
    sha3::Keccak256::digest(data).into()
}

fn write_slot(buf: &mut [u8], slot: usize, value: &[u8; 32]) {
    let off = slot * 32;
    buf[off..off + 32].copy_from_slice(value);
}

fn u64_to_u256(v: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&v.to_be_bytes());
    out
}

fn u8_to_u256(v: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[31] = v;
    out
}

fn addr_to_u256(addr: &[u8; 20]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(addr);
    out
}

/// Decimal string → big-endian 32-byte uint256 (Polymarket token IDs are huge decimals).
fn decimal_to_u256_be(s: &str) -> [u8; 32] {
    let mut result = [0u8; 32];
    for &byte in s.as_bytes() {
        if byte < b'0' || byte > b'9' { continue; }
        let digit = byte - b'0';
        let mut carry = digit as u16;
        for b in result.iter_mut().rev() {
            let val = (*b as u16) * 10 + carry;
            *b   = val as u8;
            carry = val >> 8;
        }
    }
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sim_entry_fills_at_quoted_price() {
        let fill = place_entry_order(
            &RunMode::Simulation, "", false, Side::Yes, 10.0, 0.40,
        ).await.unwrap();
        assert_eq!(fill.token_price, 0.40);
        assert_eq!(fill.size_usdc, 10.0);
        assert!((fill.size_tokens - 25.0).abs() < 1e-9);
        assert_eq!(fill.status, FillStatus::Simulated);
    }

    #[tokio::test]
    async fn sim_close_computes_notional_from_tokens() {
        let fill = place_close_order(
            &RunMode::Simulation, "", false, Side::Yes, 25.0, 0.60,
        ).await.unwrap();
        assert_eq!(fill.token_price, 0.60);
        assert!((fill.size_usdc - 15.0).abs() < 1e-9);
        assert_eq!(fill.size_tokens, 25.0);
    }

    #[tokio::test]
    async fn live_entry_fails_without_env_vars() {
        std::env::remove_var("POLY_API_KEY");
        assert!(
            place_entry_order(&RunMode::Live, "", false, Side::Yes, 10.0, 0.40)
                .await.is_err()
        );
    }

    #[tokio::test]
    async fn live_close_fails_without_env_vars() {
        std::env::remove_var("POLY_API_KEY");
        assert!(
            place_close_order(&RunMode::Live, "", false, Side::Yes, 25.0, 0.60)
                .await.is_err()
        );
    }

    #[test]
    fn decimal_to_u256_roundtrip_small() {
        let bytes = decimal_to_u256_be("256");
        assert_eq!(bytes[30], 1);
        assert_eq!(bytes[31], 0);
        assert!(bytes[..30].iter().all(|&b| b == 0));
    }

    #[test]
    fn eip712_domain_separator_deterministic() {
        let d1 = domain_separator(false);
        let d2 = domain_separator(false);
        assert_eq!(d1, d2);
        assert_ne!(domain_separator(false), domain_separator(true));
    }
}
