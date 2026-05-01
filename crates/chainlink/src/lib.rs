//! Cliente de Chainlink Data Streams (Crypto Streams v3) para BTC/USD.
//!
//! Expone una API mínima para el resto del bot:
//!   * `ChainlinkBtcPrice` — un snapshot decodificado del report v3.
//!   * `ChainlinkClient::get_btc_price_now()` — obtiene el último report
//!     firmado y devuelve `benchmark_price`, `bid` y `ask` ya escalados a USD.
//!
//! Autenticación: HMAC-SHA256 sobre `method + path + sha256(body) + api_key + ts_ms`.
//! Las credenciales se leen por env var (`CHAINLINK_API_KEY`, `CHAINLINK_API_SECRET`)
//! o se inyectan vía `from_config(...)`. Nunca se logguean en claro.

use anyhow::{anyhow, Context, Result};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tracing::{debug, warn};

type HmacSha256 = Hmac<Sha256>;

/// Endpoint REST por defecto de Data Streams (mainnet de la red de oráculos).
pub const DEFAULT_ENDPOINT: &str = "https://api.dataengine.chain.link";

/// Crypto streams v3: los precios vienen como int192 con 18 decimales.
const REPORT_DECIMALS: i32 = 18;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainlinkBtcPrice {
    /// `observationsTimestamp` del report, en nanosegundos.
    pub ts_ns: i64,
    /// Precio canónico BTC/USD según Chainlink, ya escalado a USD.
    pub benchmark_price: f64,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ChainlinkConfig {
    pub endpoint: String,
    pub btc_usd_feed_id: String,
    pub api_key: String,
    pub api_secret: String,
    pub request_timeout_ms: u64,
}

pub struct ChainlinkClient {
    cfg: ChainlinkConfig,
    http: Client,
}

impl ChainlinkClient {
    /// Construye el cliente leyendo credenciales del entorno:
    ///   * `CHAINLINK_API_KEY`         (obligatoria)
    ///   * `CHAINLINK_API_SECRET`      (obligatoria)
    ///   * `CHAINLINK_BTC_USD_FEED_ID` (obligatoria; bytes32 en hex 0x...)
    ///   * `CHAINLINK_ENDPOINT`        (opcional; default mainnet)
    pub fn from_env() -> Result<Self> {
        let endpoint = std::env::var("CHAINLINK_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
        let btc_usd_feed_id = std::env::var("CHAINLINK_BTC_USD_FEED_ID")
            .context("CHAINLINK_BTC_USD_FEED_ID no definida")?;
        let api_key = std::env::var("CHAINLINK_API_KEY")
            .context("CHAINLINK_API_KEY no definida")?;
        let api_secret = std::env::var("CHAINLINK_API_SECRET")
            .context("CHAINLINK_API_SECRET no definida")?;
        Self::from_config(ChainlinkConfig {
            endpoint,
            btc_usd_feed_id,
            api_key,
            api_secret,
            request_timeout_ms: 2000,
        })
    }

    pub fn from_config(cfg: ChainlinkConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_millis(cfg.request_timeout_ms))
            .build()
            .context("no se pudo crear el cliente HTTP")?;
        Ok(Self { cfg, http })
    }

    /// Devuelve el último report v3 disponible para el feed configurado.
    /// Hasta 3 intentos con backoff 100/250/500 ms.
    pub async fn get_btc_price_now(&self) -> Result<ChainlinkBtcPrice> {
        let backoffs = [100u64, 250, 500];
        let mut last_err: Option<anyhow::Error> = None;
        for (i, ms) in backoffs.iter().enumerate() {
            match self.fetch_latest_report().await {
                Ok(p) => return Ok(p),
                Err(e) => {
                    warn!("chainlink: intento {} fallido: {e}", i + 1);
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(*ms)).await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("chainlink: error desconocido")))
    }

    async fn fetch_latest_report(&self) -> Result<ChainlinkBtcPrice> {
        let path = format!("/api/v1/reports/latest?feedID={}", self.cfg.btc_usd_feed_id);
        let url = format!("{}{}", self.cfg.endpoint, path);
        let method = "GET";
        let body: &[u8] = b"";

        // Cadena a firmar: method + path + sha256(body) + api_key + timestamp_ms.
        // Si Data Streams cambia el formato, este es el único punto a tocar.
        let body_sha256_hex = hex::encode(Sha256::digest(body));
        let timestamp_ms = chrono::Utc::now().timestamp_millis().to_string();
        let to_sign = format!(
            "{}{}{}{}{}",
            method, path, body_sha256_hex, self.cfg.api_key, timestamp_ms
        );
        let mut mac = HmacSha256::new_from_slice(self.cfg.api_secret.as_bytes())
            .map_err(|e| anyhow!("hmac key inválida: {e}"))?;
        mac.update(to_sign.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.cfg.api_key)
            .header("X-Authorization-Timestamp", &timestamp_ms)
            .header("X-Authorization-Signature-SHA256", &signature)
            .send()
            .await
            .context("chainlink: la petición HTTP falló")?;

        let status = resp.status();
        let text = resp.text().await.context("chainlink: respuesta sin cuerpo")?;
        if !status.is_success() {
            return Err(anyhow!("chainlink HTTP {}: {}", status, snippet(&text)));
        }
        let body: serde_json::Value =
            serde_json::from_str(&text).context("chainlink: JSON inválido")?;
        debug!(
            "chainlink: respuesta keys={:?}",
            body.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>())
        );

        decode_report(&body)
    }
}

/// Trunca un texto largo para que el log sea legible.
fn snippet(s: &str) -> String {
    let max = 200usize;
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

/// Decodifica un report v3. Acepta dos formas:
///   * Campos decodificados directamente en JSON (`benchmarkPrice`, `bid`, `ask`).
///   * El blob ABI hex `fullReport` que llega en la respuesta firmada.
///
/// Estructura ABI v3 (9 campos fixed-size, 32 bytes cada uno = 288 bytes):
///   feedId (bytes32) | validFromTimestamp (u32) | observationsTimestamp (u32)
///   nativeFee (u192) | linkFee (u192) | expiresAt (u32)
///   benchmarkPrice (i192) | bid (i192) | ask (i192)
fn decode_report(body: &serde_json::Value) -> Result<ChainlinkBtcPrice> {
    let report = body
        .get("report")
        .ok_or_else(|| anyhow!("chainlink: falta el campo 'report'"))?;

    if let Some(p) = decode_decoded_fields(report) {
        return Ok(p);
    }

    let full_hex = report
        .get("fullReport")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow!("chainlink: 'fullReport' no presente y no hay campos decodificados")
        })?;
    decode_full_report_hex(full_hex)
}

fn decode_decoded_fields(report: &serde_json::Value) -> Option<ChainlinkBtcPrice> {
    let bp = decimal_field(report.get("benchmarkPrice")?)?;
    let ts_secs = report
        .get("observationsTimestamp")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    Some(ChainlinkBtcPrice {
        ts_ns: ts_secs.saturating_mul(1_000_000_000),
        benchmark_price: bp,
        bid: report.get("bid").and_then(decimal_field),
        ask: report.get("ask").and_then(decimal_field),
    })
}

fn decimal_field(v: &serde_json::Value) -> Option<f64> {
    if let Some(s) = v.as_str() {
        if let Ok(n) = s.parse::<i128>() {
            return Some(scale_decimals(n));
        }
        if let Ok(f) = s.parse::<f64>() {
            return Some(f);
        }
    }
    v.as_f64()
}

fn scale_decimals(n: i128) -> f64 {
    n as f64 / 10f64.powi(REPORT_DECIMALS)
}

fn decode_full_report_hex(full: &str) -> Result<ChainlinkBtcPrice> {
    let trimmed = full.trim_start_matches("0x");
    let bytes = hex::decode(trimmed).context("chainlink: fullReport hex inválido")?;

    // El blob v3 puro son 9 slots de 32 bytes (288 B). Si llega envuelto en un
    // sobre firmado más largo, el bloque del report v3 está al final → tomamos
    // los últimos 288 bytes como heurística.
    if bytes.len() < 288 {
        return Err(anyhow!("chainlink: fullReport de {} bytes (<288)", bytes.len()));
    }
    let off = bytes.len() - 288;
    let slot = |i: usize| -> &[u8] { &bytes[off + i * 32..off + (i + 1) * 32] };

    let observations_ts = u32_from_slot(slot(2)) as i64;
    let benchmark_raw = i192_from_slot(slot(6));
    let bid_raw = i192_from_slot(slot(7));
    let ask_raw = i192_from_slot(slot(8));

    Ok(ChainlinkBtcPrice {
        ts_ns: observations_ts.saturating_mul(1_000_000_000),
        benchmark_price: scale_decimals(benchmark_raw),
        bid: Some(scale_decimals(bid_raw)),
        ask: Some(scale_decimals(ask_raw)),
    })
}

fn u32_from_slot(slot: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&slot[28..32]);
    u32::from_be_bytes(buf)
}

/// int192 codificado en un slot de 32 bytes (right-aligned, complemento a dos
/// con sign-extend desde el byte 8 hasta el 31).
fn i192_from_slot(slot: &[u8]) -> i128 {
    // Tomamos los 16 bytes menos significativos (i128). Para BTC/USD esto siempre
    // entra de sobra: 1e8 USD escalado a 1e18 son ~1e26 < 2^96, lejos de i128::MAX.
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&slot[16..32]);
    let low = i128::from_be_bytes(buf);
    // Si los bytes superiores indican negativo, propagamos el signo.
    let negative = slot[8] & 0x80 != 0;
    if negative && slot[16] & 0x80 == 0 {
        low - (1i128 << 127) - (1i128 << 127)
    } else {
        low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_decoded_json_response() {
        let v = serde_json::json!({
            "report": {
                "benchmarkPrice": "67250120000000000000000",
                "bid":             "67249000000000000000000",
                "ask":             "67251000000000000000000",
                "observationsTimestamp": 1714000000_i64,
            }
        });
        let p = decode_report(&v).unwrap();
        assert!((p.benchmark_price - 67250.12).abs() < 1e-2);
        assert!(p.bid.unwrap() < p.benchmark_price);
        assert!(p.ask.unwrap() > p.benchmark_price);
        assert_eq!(p.ts_ns, 1714000000i64 * 1_000_000_000);
    }

    #[test]
    fn scales_with_18_decimals() {
        let s = scale_decimals(1_000_000_000_000_000_000); // 1.0
        assert!((s - 1.0).abs() < 1e-12);
    }
}
