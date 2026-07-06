//! z.ai (Zhipu/BigModel) quota collection — port of macOS `ZaiCollector.swift`.
//!
//! Endpoint: `GET https://api.z.ai/api/monitor/usage/quota/limit`
//! (env `Z_AI_API_HOST` swaps the host for BigModel CN; `Z_AI_QUOTA_URL`
//! overrides the whole URL).
//! Auth: `Authorization: Bearer <apiKey>` — env `Z_AI_API_KEY` or the
//! Settings-stored `zai_api_key`.
//!
//! Response: `{ "data": { "limits": [{ "type": "TOKENS_LIMIT"|"TIME_LIMIT",
//! "usage": int, "remaining": int, "nextResetTime": <ms epoch> }],
//! "planName": "..." } }`. Each limit becomes a tier (quota = usage +
//! remaining); the primary (first) limit drives the top-level gauge.

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const DEFAULT_HOST: &str = "api.z.ai";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct Envelope {
    #[serde(default)]
    data: Data,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Data {
    #[serde(default)]
    limits: Vec<Limit>,
    // Mac reads planName → plan → plan_type; mirror with serde aliases.
    #[serde(default, rename = "planName", alias = "plan", alias = "plan_type")]
    plan_name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Limit {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    usage: i64,
    #[serde(default)]
    remaining: i64,
    #[serde(default, rename = "nextResetTime")]
    next_reset_time: Option<i64>, // epoch milliseconds
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let api_key = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[z.ai] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_quota(&api_key).await?;
    let env: Envelope =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    Ok(Some(map_to_snapshot(&env.data)))
}

fn resolve_key() -> Option<String> {
    if let Ok(k) = std::env::var("Z_AI_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.zai_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn quota_url() -> String {
    if let Ok(url) = std::env::var("Z_AI_QUOTA_URL") {
        if !url.trim().is_empty() {
            return url.trim().to_string();
        }
    }
    let host = std::env::var("Z_AI_API_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.to_string());
    format!("https://{host}/api/monitor/usage/quota/limit")
}

async fn fetch_quota(api_key: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(quota_url())
        .header("Authorization", format!("Bearer {api_key}"))
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

fn ms_to_rfc3339(ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|d| d.to_rfc3339())
}

fn tier_name(kind: &str) -> String {
    match kind {
        "TOKENS_LIMIT" => "Tokens".to_string(),
        "TIME_LIMIT" => "Time".to_string(),
        other => other.to_string(),
    }
}

fn map_to_snapshot(data: &Data) -> QuotaSnapshot {
    let tiers: Vec<TierEntry> = data
        .limits
        .iter()
        .map(|l| {
            let total = l.usage + l.remaining;
            TierEntry {
                name: tier_name(&l.kind),
                quota: if total > 0 { total } else { l.usage },
                remaining: l.remaining,
                reset_time: l.next_reset_time.and_then(ms_to_rfc3339),
            }
        })
        .collect();
    let primary = data.limits.first();
    let total = primary.map(|p| p.usage + p.remaining).unwrap_or(0);
    QuotaSnapshot {
        plan_type: data.plan_name.clone().unwrap_or_default(),
        remaining: primary.map(|p| p.remaining).unwrap_or(0),
        quota: if total > 0 {
            total
        } else {
            primary.map(|p| p.usage).unwrap_or(0)
        },
        session_reset: primary
            .and_then(|p| p.next_reset_time)
            .and_then(ms_to_rfc3339),
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "data": {
            "planName": "Pro",
            "limits": [
                {"type":"TOKENS_LIMIT","usage":300,"remaining":700,"nextResetTime":0},
                {"type":"TIME_LIMIT","usage":2,"remaining":8}
            ]
        }
    }"#;

    #[test]
    fn parses_and_maps_limits_to_tiers() {
        let env: Envelope = serde_json::from_str(SAMPLE).unwrap();
        let snap = map_to_snapshot(&env.data);
        assert_eq!(snap.plan_type, "Pro");
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "Tokens");
        assert_eq!(snap.tiers[0].quota, 1000); // 300 + 700
        assert_eq!(snap.tiers[0].remaining, 700);
        assert_eq!(snap.tiers[1].name, "Time");
        // Top-level mirrors the primary (first) limit.
        assert_eq!(snap.quota, 1000);
        assert_eq!(snap.remaining, 700);
    }

    #[test]
    fn converts_ms_reset_to_rfc3339() {
        let env: Envelope = serde_json::from_str(SAMPLE).unwrap();
        let snap = map_to_snapshot(&env.data);
        // nextResetTime 0 ms → 1970-01-01T00:00:00Z
        assert_eq!(
            snap.session_reset.as_deref(),
            Some("1970-01-01T00:00:00+00:00")
        );
        assert!(snap.tiers[0].reset_time.is_some());
        assert!(snap.tiers[1].reset_time.is_none()); // no nextResetTime on TIME_LIMIT
    }

    #[test]
    fn plan_name_aliases_and_empty() {
        let e: Envelope = serde_json::from_str(r#"{"data":{"plan":"Lite","limits":[]}}"#).unwrap();
        assert_eq!(map_to_snapshot(&e.data).plan_type, "Lite");
        let e: Envelope = serde_json::from_str(r#"{"data":{"limits":[]}}"#).unwrap();
        assert_eq!(map_to_snapshot(&e.data).plan_type, "");
    }

    #[test]
    fn unknown_limit_type_kept_verbatim() {
        assert_eq!(tier_name("WEIRD_LIMIT"), "WEIRD_LIMIT");
    }

    #[test]
    fn quota_url_default_and_override() {
        // (env-free assertion — the default host, no env set in this test binary)
        assert!(quota_url().starts_with("https://") && quota_url().contains("/api/monitor/"));
    }
}
