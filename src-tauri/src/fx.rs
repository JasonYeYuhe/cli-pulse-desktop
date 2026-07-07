//! Foreign-exchange rates for multi-currency cost display.
//!
//! Costs are computed and stored in **USD** everywhere; this module fetches
//! USD→{other} daily reference rates so the UI can *display* costs in the user's
//! chosen currency. Source: `open.er-api.com` (free, no API key, daily-updated,
//! returns every currency in one response), overridable via `FX_RATES_URL`.
//!
//! Privacy: this is **read-only public data** — the request sends nothing about
//! the user (no auth, no body, no identifiers), it only reads published rates.
//! Cached ~6 h (rates move once a day); on a fetch failure we serve the last
//! good rates (even if stale) so a transient network blip doesn't blank the UI.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

const DEFAULT_URL: &str = "https://open.er-api.com/v6/latest/USD";
const TTL: Duration = Duration::from_secs(6 * 3600);
const TIMEOUT: Duration = Duration::from_secs(15);

/// USD-based rate table sent to the frontend. `rates[c]` is how many units of
/// currency `c` equal 1 USD (e.g. `rates["CNY"] = 7.2`).
#[derive(Debug, Clone, Serialize)]
pub struct FxRates {
    pub base: String,
    pub rates: HashMap<String, f64>,
    /// Human-readable "as of" from the source (best-effort; may be empty).
    pub as_of: String,
}

type FxCache = Option<(Instant, FxRates)>;
static CACHE: Lazy<Mutex<FxCache>> = Lazy::new(|| Mutex::new(None));

#[derive(Deserialize)]
struct ErApiResponse {
    #[serde(default)]
    result: String,
    #[serde(default)]
    base_code: String,
    #[serde(default)]
    rates: HashMap<String, f64>,
    #[serde(default)]
    time_last_update_utc: String,
}

/// Cached USD rate table. Serves from cache within the TTL; on a fetch failure,
/// falls back to the last good (possibly stale) rates before erroring.
pub async fn get_rates() -> Result<FxRates, String> {
    if let Some((at, rates)) = CACHE.lock().unwrap().clone() {
        if at.elapsed() < TTL {
            return Ok(rates);
        }
    }
    match fetch().await {
        Ok(rates) => {
            *CACHE.lock().unwrap() = Some((Instant::now(), rates.clone()));
            Ok(rates)
        }
        Err(e) => {
            if let Some((_, rates)) = CACHE.lock().unwrap().clone() {
                log::debug!("[fx] fetch failed ({e}); serving stale cached rates");
                return Ok(rates);
            }
            Err(e)
        }
    }
}

async fn fetch() -> Result<FxRates, String> {
    let url = std::env::var("FX_RATES_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let body = client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?
        .error_for_status()
        .map_err(|e| format!("http: {e}"))?
        .text()
        .await
        .map_err(|e| format!("body: {e}"))?;
    parse(&body)
}

/// Parse an `open.er-api.com` response into a USD rate table. Pure + tested.
pub fn parse(body: &str) -> Result<FxRates, String> {
    let resp: ErApiResponse = serde_json::from_str(body).map_err(|e| format!("parse: {e}"))?;
    // The API sets `result: "success"` on good responses; treat a present-but-
    // non-success result as an error, but tolerate its absence (custom sources).
    if !resp.result.is_empty() && resp.result != "success" {
        return Err(format!("fx source result: {}", resp.result));
    }
    if resp.rates.is_empty() {
        return Err("fx response had no rates".to_string());
    }
    Ok(FxRates {
        base: if resp.base_code.is_empty() {
            "USD".to_string()
        } else {
            resp.base_code
        },
        rates: resp.rates,
        as_of: resp.time_last_update_utc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_er_api_success() {
        let body = r#"{
            "result":"success","base_code":"USD",
            "time_last_update_utc":"Tue, 08 Jul 2026 00:00:01 +0000",
            "rates":{"USD":1.0,"CNY":7.21,"EUR":0.92,"JPY":161.3,"GBP":0.78}
        }"#;
        let fx = parse(body).unwrap();
        assert_eq!(fx.base, "USD");
        assert_eq!(fx.rates.get("CNY").copied(), Some(7.21));
        assert!(fx.as_of.contains("2026"));
    }

    #[test]
    fn rejects_error_result() {
        let body = r#"{"result":"error","error-type":"invalid-key","rates":{}}"#;
        assert!(parse(body).is_err());
    }

    #[test]
    fn rejects_empty_rates() {
        assert!(parse(r#"{"result":"success","rates":{}}"#).is_err());
    }

    #[test]
    fn tolerates_missing_result_and_base() {
        // A custom FX_RATES_URL might omit `result`/`base_code`; default base to USD.
        let fx = parse(r#"{"rates":{"CNY":7.0}}"#).unwrap();
        assert_eq!(fx.base, "USD");
        assert_eq!(fx.rates.get("CNY").copied(), Some(7.0));
    }
}
