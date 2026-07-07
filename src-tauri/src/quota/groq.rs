//! Groq throughput collection — port of macOS `GroqCollector` (itself derived
//! from steipete/CodexBar, MIT).
//!
//! Groq (the inference company; NOT xAI's Grok) exposes a **Prometheus**
//! metrics endpoint with throughput RATES (req/sec, tok/sec) — no quota / cost
//! / balance — so it's **status-only**: `"X req/min · Y tok/min"`.
//!
//! Endpoint: `GET {base}/metrics/prometheus/api/v1/query?query=<PromQL>` (base
//! from env `GROQ_API_URL`, default `https://api.groq.com/v1`). Auth:
//! `Authorization: Bearer <apiKey>` — env `GROQ_API_KEY` or the Settings-stored
//! `groq_api_key`. Four rate queries run concurrently, each 10s-bounded.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::{CollectorError, QuotaSnapshot};

const DEFAULT_BASE: &str = "https://api.groq.com/v1";
const TIMEOUT: Duration = Duration::from_secs(10);

const Q_REQUESTS: &str = "sum(model_project_id_status_code:requests:rate5m)";
const Q_TOKENS_IN: &str = "sum(model_project_id:tokens_in:rate5m)";
const Q_TOKENS_OUT: &str = "sum(model_project_id:tokens_out:rate5m)";
const Q_CACHE_HITS: &str = "sum(model_project_id:prompt_cache_hits:rate5m)";

#[derive(Debug, Clone, Default, Deserialize)]
struct PromResponse {
    #[serde(default)]
    status: String,
    #[serde(default)]
    data: Option<PromData>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PromData {
    #[serde(default)]
    result: Vec<PromSeries>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PromSeries {
    /// Prometheus instant value: `[<unix ts: number>, "<value: string>"]` —
    /// a mixed number/string array, so decode as raw JSON values.
    #[serde(default)]
    value: Option<Vec<Value>>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let key = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[Groq] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let base = resolve_base();
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;

    // 4 concurrent rate queries ⇒ ~10s wall-clock. Any failure propagates.
    let (requests, tokens_in, tokens_out, cache_hits) = tokio::try_join!(
        query_scalar(&client, &base, &key, Q_REQUESTS),
        query_scalar(&client, &base, &key, Q_TOKENS_IN),
        query_scalar(&client, &base, &key, Q_TOKENS_OUT),
        query_scalar(&client, &base, &key, Q_CACHE_HITS),
    )?;

    Ok(Some(build_status(
        requests, tokens_in, tokens_out, cache_hits,
    )))
}

fn resolve_key() -> Option<String> {
    if let Ok(k) = std::env::var("GROQ_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.groq_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn resolve_base() -> String {
    std::env::var("GROQ_API_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

async fn query_scalar(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    query: &str,
) -> Result<f64, CollectorError> {
    let url = format!("{base}/metrics/prometheus/api/v1/query");
    let resp = client
        .get(&url)
        .query(&[("query", query)])
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| CollectorError::Http(format!("request: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(CollectorError::Http(format!("HTTP {}", status.as_u16())));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| CollectorError::Http(format!("body: {e}")))?;
    parse_scalar(&body)
}

fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Prometheus instant-query envelope → the summed last-value of each series.
/// An empty result set is a graceful 0; a non-"success" status is an error.
fn parse_scalar(body: &str) -> Result<f64, CollectorError> {
    let resp: PromResponse =
        serde_json::from_str(body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    if resp.status != "success" {
        return Err(CollectorError::SchemaOrIo(format!(
            "Groq: {}",
            resp.error.as_deref().unwrap_or("query failed")
        )));
    }
    let sum = resp
        .data
        .map(|d| {
            d.result
                .iter()
                .filter_map(|s| {
                    s.value
                        .as_ref()
                        .and_then(|v| v.last())
                        .and_then(value_to_f64)
                })
                .sum::<f64>()
        })
        .unwrap_or(0.0);
    Ok(sum)
}

/// ≥100 → 0dp, ≥10 → 1dp, else 2dp; exactly 0 / non-finite → "0".
fn fmt_decimal(value: f64) -> String {
    if !value.is_finite() || value == 0.0 {
        return "0".to_string();
    }
    if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

fn build_status(
    requests_per_sec: f64,
    input_tok_per_sec: f64,
    output_tok_per_sec: f64,
    cache_hits_per_sec: f64,
) -> QuotaSnapshot {
    let req_per_min = requests_per_sec.max(0.0) * 60.0;
    let tok_per_min = (input_tok_per_sec + output_tok_per_sec).max(0.0) * 60.0;
    let cache_per_min = cache_hits_per_sec.max(0.0) * 60.0;

    let mut status = format!(
        "{} req/min · {} tok/min",
        fmt_decimal(req_per_min),
        fmt_decimal(tok_per_min)
    );
    if cache_hits_per_sec > 0.0 {
        status.push_str(&format!(" · {} cache/min", fmt_decimal(cache_per_min)));
    }

    // Throughput rates only — no quota gauge.
    QuotaSnapshot {
        status_text: Some(status),
        plan_type: "API key".to_string(),
        remaining: 0,
        quota: 0,
        session_reset: None,
        tiers: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prometheus_scalar_and_sums_series() {
        // Two series; value is [ts, "string"] — sum the last (string) values.
        let body = r#"{"status":"success","data":{"result":[
            {"value":[1800000000,"1.5"]},
            {"value":[1800000000,"2.5"]}
        ]}}"#;
        assert_eq!(parse_scalar(body).unwrap(), 4.0);
    }

    #[test]
    fn empty_result_is_zero_and_number_value_ok() {
        assert_eq!(
            parse_scalar(r#"{"status":"success","data":{"result":[]}}"#).unwrap(),
            0.0
        );
        // numeric (non-string) value also decodes.
        let body = r#"{"status":"success","data":{"result":[{"value":[1,3.0]}]}}"#;
        assert_eq!(parse_scalar(body).unwrap(), 3.0);
    }

    #[test]
    fn non_success_status_is_error() {
        assert!(parse_scalar(r#"{"status":"error","error":"bad query"}"#).is_err());
    }

    #[test]
    fn build_status_formats_rates_per_minute() {
        // 2 req/s → 120/min; (10+5) tok/s → 900/min; no cache.
        let snap = build_status(2.0, 10.0, 5.0, 0.0);
        assert_eq!(snap.plan_type, "API key");
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        assert_eq!(
            snap.status_text.as_deref(),
            Some("120 req/min · 900 tok/min")
        );
    }

    #[test]
    fn build_status_appends_cache_when_positive() {
        // 0.1 req/s → 6.00/min (2dp region); cache 0.5/s → 30.0/min (1dp region).
        let snap = build_status(0.1, 0.0, 0.0, 0.5);
        assert_eq!(
            snap.status_text.as_deref(),
            Some("6.00 req/min · 0 tok/min · 30.0 cache/min")
        );
    }

    #[test]
    fn fmt_decimal_precision_buckets() {
        assert_eq!(fmt_decimal(0.0), "0");
        assert_eq!(fmt_decimal(5.25), "5.25");
        assert_eq!(fmt_decimal(42.5), "42.5");
        assert_eq!(fmt_decimal(1234.0), "1234");
    }
}
