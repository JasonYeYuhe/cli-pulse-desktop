//! Volcano Engine (火山引擎 / 豆包) Ark usage collection — port of macOS
//! `VolcanoEngineCollector`.
//!
//! Endpoint: `GET https://ark.cn-beijing.volces.com/api/v3/models` (env
//! `ARK_API_HOST` swaps the host). Auth: `Authorization: Bearer <apiKey>` —
//! env `ARK_API_KEY` / `VOLC_ACCESSKEY` / `VOLCANO_ENGINE_API_KEY`, or the
//! Settings-stored `volcano_api_key`.
//!
//! Dual-mode: when the response carries `{total, remaining}` it's a REAL quota
//! (an "Ark Plan" gauge); otherwise the models-list endpoint is a
//! connectivity probe and it degrades to a **status-only** `status_text` line
//! (`"N models available"` / `"Connected"`), with no gauge drawn.

use std::time::Duration;

use serde_json::Value;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const DEFAULT_HOST: &str = "ark.cn-beijing.volces.com";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq)]
struct VolcanoUsage {
    model_count: usize,
    quota: i64,
    remaining: i64,
    end_time: Option<String>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[Volcano Engine] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_usage(&token).await?;
    let usage = parse_response(&body)?;
    Ok(Some(map_to_snapshot(&usage)))
}

fn resolve_key() -> Option<String> {
    for env in ["ARK_API_KEY", "VOLC_ACCESSKEY", "VOLCANO_ENGINE_API_KEY"] {
        if let Ok(k) = std::env::var(env) {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.volcano_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn usage_url() -> String {
    let host = std::env::var("ARK_API_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.to_string());
    format!("https://{host}/api/v3/models")
}

async fn fetch_usage(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(usage_url())
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| CollectorError::Http(format!("request: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(CollectorError::Http(format!("HTTP {}", status.as_u16())));
    }
    resp.text()
        .await
        .map_err(|e| CollectorError::Http(format!("body: {e}")))
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Three shapes, in the Mac's precedence order: top-level `{total,remaining}`,
/// then a `data` models array, then a `result`/`Response` wrapper.
fn parse_response(body: &str) -> Result<VolcanoUsage, CollectorError> {
    let json: Value =
        serde_json::from_str(body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;

    // Shape 1: top-level quota fields.
    if let Some(total) = json.get("total").and_then(Value::as_i64) {
        return Ok(VolcanoUsage {
            model_count: 0,
            quota: total,
            remaining: json.get("remaining").and_then(Value::as_i64).unwrap_or(0),
            end_time: str_at(&json, "end_time").or_else(|| str_at(&json, "reset_time")),
        });
    }
    // Shape 2: models list → connectivity probe.
    if let Some(arr) = json.get("data").and_then(Value::as_array) {
        return Ok(VolcanoUsage {
            model_count: arr.len(),
            quota: 0,
            remaining: 0,
            end_time: None,
        });
    }
    // Shape 3: `result` / `Response` wrapper with quota fields.
    if let Some(res) = json
        .get("result")
        .or_else(|| json.get("Response"))
        .filter(|v| v.is_object())
    {
        return Ok(VolcanoUsage {
            model_count: 0,
            quota: res.get("total").and_then(Value::as_i64).unwrap_or(0),
            remaining: res.get("remaining").and_then(Value::as_i64).unwrap_or(0),
            end_time: str_at(res, "end_time").or_else(|| str_at(res, "reset_time")),
        });
    }
    Err(CollectorError::SchemaOrIo(
        "Volcano Engine: unexpected response structure".to_string(),
    ))
}

fn map_to_snapshot(u: &VolcanoUsage) -> QuotaSnapshot {
    let mut tiers: Vec<TierEntry> = Vec::new();
    let (quota, remaining, session_reset, status_text) = if u.quota > 0 {
        let used = (u.quota - u.remaining).max(0);
        tiers.push(TierEntry {
            name: "Ark Plan".to_string(),
            quota: u.quota,
            remaining: u.remaining,
            reset_time: u.end_time.clone(),
        });
        (
            u.quota,
            u.remaining,
            u.end_time.clone(),
            format!("{}/{} used", used, u.quota),
        )
    } else if u.model_count > 0 {
        (0, 0, None, format!("{} models available", u.model_count))
    } else {
        (0, 0, None, "Connected".to_string())
    };

    QuotaSnapshot {
        status_text: Some(status_text),
        plan_type: String::new(),
        remaining,
        quota,
        session_reset,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_quota_shape_makes_ark_plan_tier() {
        let u =
            parse_response(r#"{"total":1000,"remaining":300,"end_time":"2026-08-01T00:00:00Z"}"#)
                .unwrap();
        assert_eq!(u.quota, 1000);
        assert_eq!(u.remaining, 300);
        let snap = map_to_snapshot(&u);
        assert_eq!(snap.quota, 1000);
        assert_eq!(snap.remaining, 300);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Ark Plan");
        assert_eq!(snap.tiers[0].remaining, 300);
        assert_eq!(snap.status_text.as_deref(), Some("700/1000 used"));
        assert_eq!(snap.session_reset.as_deref(), Some("2026-08-01T00:00:00Z"));
    }

    #[test]
    fn models_list_shape_is_status_only() {
        let u = parse_response(r#"{"data":[{"id":"doubao-pro"},{"id":"doubao-lite"}]}"#).unwrap();
        assert_eq!(u.model_count, 2);
        let snap = map_to_snapshot(&u);
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.status_text.as_deref(), Some("2 models available"));
    }

    #[test]
    fn result_wrapper_and_reset_time_alias() {
        let u =
            parse_response(r#"{"result":{"total":50,"remaining":50,"reset_time":"R"}}"#).unwrap();
        assert_eq!(u.quota, 50);
        assert_eq!(
            map_to_snapshot(&u).tiers[0].reset_time.as_deref(),
            Some("R")
        );
        // Response (capitalized) wrapper also accepted.
        let u = parse_response(r#"{"Response":{"total":10,"remaining":4}}"#).unwrap();
        assert_eq!(u.quota, 10);
    }

    #[test]
    fn zero_total_degrades_to_connected() {
        let u = parse_response(r#"{"total":0,"remaining":0}"#).unwrap();
        let snap = map_to_snapshot(&u);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.status_text.as_deref(), Some("Connected"));
    }

    #[test]
    fn unknown_shape_is_error() {
        assert!(parse_response(r#"{"unrelated":1}"#).is_err());
    }

    #[test]
    fn usage_url_default_path() {
        assert!(usage_url().ends_with("/api/v3/models"));
        assert!(usage_url().starts_with("https://"));
    }
}
