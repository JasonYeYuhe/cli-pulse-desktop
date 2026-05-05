//! OpenRouter quota collection — port of macOS
//! `Collectors/OpenRouterCollector.swift`.
//!
//! Auth source: env `OPENROUTER_API_KEY`. Optional env
//! `OPENROUTER_API_URL` overrides default `https://openrouter.ai/api/v1`.
//!
//! Endpoints:
//! - `GET <base>/credits` — required, 15s timeout. Response:
//!   `{ data: { total_credits, total_usage } }`.
//! - `GET <base>/key` — optional, 3s timeout, non-fatal failure.
//!   Response: `{ data: { limit, usage, rate_limit: { ... } } }`.
//!
//! Tiers emitted: "Credits" (always), "Key Limit" (if `keyInfo.limit > 0`).
//!
//! Scaling: $1 = 100,000 units (matches Mac line 119-122). KNOWN BUG:
//! `provider_quotas.{quota,remaining}` columns are i32, so balances
//! over ~$21k overflow on the server `INSERT` cast to integer. This
//! is a Mac-inherited bug; bigint migration is a separate v0.4.4+
//! sprint per `feedback_cli_pulse_autonomy.md` schema-change rules.
//! Realistic exposure: < 0.001% of users.

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const CREDITS_TIMEOUT: Duration = Duration::from_secs(15);
const KEY_TIMEOUT: Duration = Duration::from_secs(3);
/// $1 = 100_000 units. Matches `OpenRouterCollector.swift:119`.
const SCALE: f64 = 100_000.0;

#[derive(Debug, Clone, Deserialize)]
struct CreditsEnvelope {
    data: CreditsData,
}

#[derive(Debug, Clone, Deserialize)]
struct CreditsData {
    #[serde(default)]
    total_credits: f64,
    #[serde(default)]
    total_usage: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct KeyEnvelope {
    data: KeyData,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct KeyData {
    #[serde(default)]
    limit: Option<f64>,
    #[serde(default)]
    usage: Option<f64>,
}

/// Collect OpenRouter quota. v0.4.6 — credential read priority for both
/// API key and base URL:
///   1. Env `OPENROUTER_API_KEY` / `OPENROUTER_API_URL` (backwards compat)
///   2. `provider_creds.json` `openrouter_api_key` / `openrouter_base_url`
///   3. None for key → silent debug skip; default for URL → openrouter.ai.
///
/// v0.4.20 return shape:
/// - `Ok(Some(snap))` — success.
/// - `Ok(None)` — no API key configured.
/// - `Err(...)` — `/credits` HTTP failure. Note: `/key` failure stays
///   non-fatal (key info is optional metadata that just enriches the
///   snapshot).
pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let saved = crate::provider_creds::load().ok();

    let api_key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            saved
                .as_ref()
                .and_then(|c| c.openrouter_api_key.clone())
                .filter(|s| !s.is_empty())
        });
    let api_key = match api_key {
        Some(k) => k,
        None => {
            log::debug!("[OpenRouter] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };

    let base_url = std::env::var("OPENROUTER_API_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            saved
                .as_ref()
                .and_then(|c| c.openrouter_base_url.clone())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    let credits = match fetch_credits(&base_url, &api_key).await {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[OpenRouter] /credits fetch failed (non-fatal): {e}");
            return Err(CollectorError::Http(format!("/credits: {e}")));
        }
    };
    let key_info = fetch_key_info(&base_url, &api_key).await.ok();
    Ok(Some(map_to_snapshot(&credits, key_info.as_ref())))
}

async fn fetch_credits(base_url: &str, api_key: &str) -> Result<CreditsData, String> {
    let url = format!("{}/credits", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(CREDITS_TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(120).collect();
        return Err(format!("HTTP {} — {}", status.as_u16(), snippet));
    }
    let env: CreditsEnvelope = resp.json().await.map_err(|e| format!("parse: {e}"))?;
    Ok(env.data)
}

async fn fetch_key_info(base_url: &str, api_key: &str) -> Result<KeyData, String> {
    let url = format!("{}/key", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(KEY_TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let env: KeyEnvelope = resp.json().await.map_err(|e| format!("parse: {e}"))?;
    Ok(env.data)
}

fn map_to_snapshot(credits: &CreditsData, key_info: Option<&KeyData>) -> QuotaSnapshot {
    let balance = (credits.total_credits - credits.total_usage).max(0.0);
    let credits_quota = scale_to_units(credits.total_credits);
    let credits_remaining = scale_to_units(balance);

    let mut tiers = Vec::with_capacity(2);
    tiers.push(TierEntry {
        name: "Credits".to_string(),
        quota: credits_quota,
        remaining: credits_remaining,
        reset_time: None,
    });

    if let Some(k) = key_info {
        if let Some(limit) = k.limit {
            if limit > 0.0 {
                let usage = k.usage.unwrap_or(0.0);
                let key_remaining = (limit - usage).max(0.0);
                tiers.push(TierEntry {
                    name: "Key Limit".to_string(),
                    quota: scale_to_units(limit),
                    remaining: scale_to_units(key_remaining),
                    reset_time: None,
                });
            }
        }
    }

    QuotaSnapshot {
        plan_type: "Credits".to_string(),
        remaining: credits_remaining,
        quota: credits_quota,
        session_reset: None,
        tiers,
    }
}

/// Scale a dollar value to integer units. Clamps to i64::MAX if a
/// pathological input overflows; the helper_sync server-side cast to
/// `integer` (i32) is the real overflow point — see module-level
/// docs about the inherited Mac bug.
fn scale_to_units(dollars: f64) -> i64 {
    let units = (dollars * SCALE).round();
    if units >= i64::MAX as f64 {
        i64::MAX
    } else if units <= 0.0 {
        0
    } else {
        units as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_credits_response() {
        let json = r#"{"data": {"total_credits": 5.43, "total_usage": 1.20}}"#;
        let env: CreditsEnvelope = serde_json::from_str(json).unwrap();
        assert!((env.data.total_credits - 5.43).abs() < 1e-9);
        assert!((env.data.total_usage - 1.20).abs() < 1e-9);
    }

    #[test]
    fn parse_key_response_full() {
        let json = r#"{"data": {"limit": 10.0, "usage": 4.5, "rate_limit": {"requests": 100, "interval": "10s"}}}"#;
        let env: KeyEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.data.limit, Some(10.0));
        assert_eq!(env.data.usage, Some(4.5));
    }

    #[test]
    fn parse_key_response_missing_fields() {
        let json = r#"{"data": {}}"#;
        let env: KeyEnvelope = serde_json::from_str(json).unwrap();
        assert!(env.data.limit.is_none());
        assert!(env.data.usage.is_none());
    }

    #[test]
    fn snapshot_credits_only() {
        let credits = CreditsData {
            total_credits: 10.0,
            total_usage: 3.0,
        };
        let snap = map_to_snapshot(&credits, None);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Credits");
        assert_eq!(snap.tiers[0].quota, 1_000_000);
        assert_eq!(snap.tiers[0].remaining, 700_000);
        assert_eq!(snap.plan_type, "Credits");
    }

    #[test]
    fn snapshot_with_key_limit() {
        let credits = CreditsData {
            total_credits: 50.0,
            total_usage: 10.0,
        };
        let key = KeyData {
            limit: Some(20.0),
            usage: Some(5.0),
        };
        let snap = map_to_snapshot(&credits, Some(&key));
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[1].name, "Key Limit");
        assert_eq!(snap.tiers[1].quota, 2_000_000);
        assert_eq!(snap.tiers[1].remaining, 1_500_000); // (20-5) * 100k
    }

    #[test]
    fn snapshot_zero_key_limit_skipped() {
        let credits = CreditsData {
            total_credits: 5.0,
            total_usage: 0.0,
        };
        let key = KeyData {
            limit: Some(0.0),
            usage: None,
        };
        let snap = map_to_snapshot(&credits, Some(&key));
        assert_eq!(snap.tiers.len(), 1); // "Key Limit" skipped
    }

    #[test]
    fn balance_clamps_negative_to_zero() {
        // Defensive: total_usage > total_credits (shouldn't happen).
        let credits = CreditsData {
            total_credits: 1.0,
            total_usage: 10.0,
        };
        let snap = map_to_snapshot(&credits, None);
        assert_eq!(snap.tiers[0].remaining, 0);
    }
}
