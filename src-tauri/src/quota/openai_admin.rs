//! OpenAI organization month-to-date spend — port of macOS `OpenAIAdminCollector`
//! (itself derived from steipete/CodexBar, MIT).
//!
//! Endpoint: `GET api.openai.com/v1/organization/costs?start_time&end_time&
//! bucket_width=1d&limit=31`, `Authorization: Bearer <admin key>`.
//! This is the ORGANIZATION cost admin API — DISTINCT from CLI Pulse's existing
//! Codex provider (the OpenAI Codex CLI). It needs an **org admin key**
//! (`sk-admin-…`); a regular `sk-…` key 401s.
//!
//! **Divergence from the Mac:** the Mac also falls back to the ubiquitous
//! `OPENAI_API_KEY` env var. A desktop GUI app doesn't inherit shell env, and a
//! non-admin `OPENAI_API_KEY` would just 401 forever — so we read **only**
//! `OPENAI_ADMIN_KEY` (or the Settings `openai_admin_key`), and a 401/403 is a
//! genuine "wrong key" error rather than expected noise.
//!
//! Status-only (`quota`/`remaining` = 0, no tiers): the signal is the
//! `status_text` "$X this month" (month-to-date spend, summed over daily
//! buckets).
//!
//! Like the Mac (which notes the admin API "occasionally 503s under load"),
//! the idempotent GET is retried **once** on a transient failure (network
//! error / 408 / 429 / 5xx) before surfacing an error.

use std::time::Duration;

use serde_json::Value;

use super::{CollectorError, QuotaSnapshot};

const COSTS_URL: &str = "https://api.openai.com/v1/organization/costs";
const TIMEOUT: Duration = Duration::from_secs(20);

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let key = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[OpenAI Admin] no admin key (OPENAI_ADMIN_KEY or Settings) — skipping");
            return Ok(None);
        }
    };
    let now = chrono::Utc::now();
    let body = fetch_costs(&key, month_start_unix(now), now.timestamp()).await?;
    let (total, currency) = parse_costs(&body)?;
    Ok(Some(build_snapshot(total, &currency)))
}

fn resolve_key() -> Option<String> {
    if let Ok(k) = std::env::var("OPENAI_ADMIN_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.openai_admin_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// First instant of `now`'s UTC month, as a Unix timestamp (seconds).
fn month_start_unix(now: chrono::DateTime<chrono::Utc>) -> i64 {
    use chrono::{Datelike, TimeZone};
    chrono::Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .map(|d| d.timestamp())
        .unwrap_or_else(|| now.timestamp())
}

/// 408 / 429 / any 5xx are worth one retry; a network send error likewise.
fn is_transient(code: u16) -> bool {
    code == 408 || code == 429 || (500..=599).contains(&code)
}

async fn fetch_costs(key: &str, start: i64, end: i64) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;

    // Retry the idempotent GET once on a transient failure (see module docs).
    let mut last: Option<CollectorError> = None;
    for attempt in 0..2u8 {
        let resp = match client
            .get(COSTS_URL)
            .query(&[
                ("start_time", start.to_string()),
                ("end_time", end.to_string()),
                ("bucket_width", "1d".to_string()),
                ("limit", "31".to_string()),
            ])
            .header("Authorization", format!("Bearer {key}"))
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // Network/timeout — transient; retry once then give up.
                last = Some(CollectorError::Http(format!("request: {e}")));
                if attempt == 0 {
                    continue;
                }
                break;
            }
        };
        let code = resp.status().as_u16();
        if code == 401 || code == 403 {
            return Err(CollectorError::Http(
                "org admin key (sk-admin-) required".to_string(),
            ));
        }
        if is_transient(code) {
            last = Some(CollectorError::Http(format!("HTTP {code}")));
            if attempt == 0 {
                continue;
            }
            break;
        }
        if !(200..=299).contains(&code) {
            return Err(CollectorError::Http(format!("HTTP {code}")));
        }
        return resp
            .text()
            .await
            .map_err(|e| CollectorError::Http(format!("body: {e}")));
    }
    Err(last.unwrap_or_else(|| CollectorError::Http("no response".to_string())))
}

/// Number tolerant of JSON number or numeric-string encodings (mirrors the
/// upstream flexible amount decode).
fn flexible_f64(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn parse_costs(body: &str) -> Result<(f64, String), CollectorError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| CollectorError::SchemaOrIo(format!("parse: {e}")))?;
    let buckets = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| CollectorError::SchemaOrIo("no data array".into()))?;

    let mut total = 0.0;
    let mut currency: Option<String> = None;
    for bucket in buckets {
        if let Some(results) = bucket.get("results").and_then(Value::as_array) {
            for r in results {
                if let Some(amount) = r.get("amount") {
                    total += flexible_f64(amount.get("value")).unwrap_or(0.0);
                    if currency.is_none() {
                        if let Some(c) = amount
                            .get("currency")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            currency = Some(c.to_string());
                        }
                    }
                }
            }
        }
    }
    Ok((
        total.max(0.0),
        currency.unwrap_or_else(|| "USD".to_string()),
    ))
}

fn build_snapshot(total: f64, currency: &str) -> QuotaSnapshot {
    let cost = total.max(0.0);
    let status = if currency.eq_ignore_ascii_case("USD") {
        format!("${cost:.2} this month")
    } else {
        format!("{currency} {cost:.2} this month")
    };
    QuotaSnapshot {
        status_text: Some(status),
        plan_type: "Admin API".to_string(),
        remaining: 0,
        quota: 0,
        session_reset: None,
        tiers: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn sums_amount_across_buckets_and_results() {
        let body = r#"{"data":[
            {"results":[{"amount":{"value":1.25,"currency":"usd"}}]},
            {"results":[{"amount":{"value":2.5,"currency":"usd"}},{"amount":{"value":0.25,"currency":"usd"}}]}
        ]}"#;
        let (total, currency) = parse_costs(body).unwrap();
        assert!((total - 4.0).abs() < 1e-9);
        assert_eq!(currency, "usd");
        assert_eq!(
            build_snapshot(total, &currency).status_text.as_deref(),
            Some("$4.00 this month") // "usd" matches USD case-insensitively
        );
    }

    #[test]
    fn empty_data_is_zero_not_error() {
        let (total, currency) = parse_costs(r#"{"data":[]}"#).unwrap();
        assert_eq!(total, 0.0);
        assert_eq!(currency, "USD");
        let snap = build_snapshot(total, &currency);
        assert_eq!(snap.plan_type, "Admin API");
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.status_text.as_deref(), Some("$0.00 this month"));
    }

    #[test]
    fn flexible_string_amount_and_non_usd_currency() {
        let body = r#"{"data":[{"results":[{"amount":{"value":"12.5","currency":"EUR"}}]}]}"#;
        let (total, currency) = parse_costs(body).unwrap();
        assert_eq!(
            build_snapshot(total, &currency).status_text.as_deref(),
            Some("EUR 12.50 this month")
        );
    }

    #[test]
    fn missing_data_array_is_schema_error() {
        assert!(matches!(
            parse_costs(r#"{"object":"page"}"#),
            Err(CollectorError::SchemaOrIo(_))
        ));
    }

    #[test]
    fn month_start_is_first_of_month_utc() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 7, 13, 45, 0).unwrap();
        let start = month_start_unix(now);
        let expected = chrono::Utc
            .with_ymd_and_hms(2026, 7, 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        assert_eq!(start, expected);
    }
}
