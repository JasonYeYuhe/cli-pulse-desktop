//! Kimi (consumer app) usage collection — port of macOS `KimiCollector`.
//!
//! DISTINCT from the `kimi_k2` collector (that's the kimi-k2.ai developer
//! api-key balance; this is the kimi.com coding-console usage).
//!
//! Endpoint: `POST www.kimi.com/apiv2/kimi.gateway.billing.v1.BillingService/GetUsages`
//! — Connect-RPC with the **JSON codec** (`Content-Type: application/json`,
//! `connect-protocol-version: 1`, body `{"scope":["FEATURE_CODING"]}`), so it's
//! an ordinary HTTP+JSON POST, not protobuf.
//! Auth: the **`kimi-auth` JWT** (Bearer + Cookie) from env `KIMI_AUTH_TOKEN` or
//! the Settings-stored `kimi_auth_token` — the value may be the raw JWT or a
//! full Cookie header (the `kimi-auth=…` value is extracted). Manual only.
//!
//! A real depleting `.quota` (raw request counts): a weekly window (headline) +
//! an optional 5-hour rate-limit window. All counts arrive as JSON *strings*.

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const USAGES_URL: &str =
    "https://www.kimi.com/apiv2/kimi.gateway.billing.v1.BillingService/GetUsages";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct UsagesResponse {
    #[serde(default)]
    usages: Vec<Usage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    detail: Option<Detail>,
    #[serde(default)]
    limits: Vec<Limit>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Limit {
    #[serde(default)]
    detail: Option<Detail>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Detail {
    #[serde(default)]
    limit: Option<String>,
    #[serde(default)]
    used: Option<String>,
    #[serde(default)]
    remaining: Option<String>,
    #[serde(default, rename = "resetTime")]
    reset_time: Option<String>,
}

impl Detail {
    fn limit_i(&self) -> Option<i64> {
        self.limit.as_deref().and_then(|s| s.trim().parse().ok())
    }
    fn used_i(&self) -> Option<i64> {
        self.used.as_deref().and_then(|s| s.trim().parse().ok())
    }
    fn remaining_i(&self) -> Option<i64> {
        self.remaining
            .as_deref()
            .and_then(|s| s.trim().parse().ok())
    }
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_token() {
        Some(t) => t,
        None => {
            log::debug!("[Kimi] no auth token (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_usages(&token).await?;
    let resp: UsagesResponse =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    let usage = resp
        .usages
        .into_iter()
        .next()
        .ok_or_else(|| CollectorError::SchemaOrIo("Kimi: no usages array".to_string()))?;
    Ok(Some(map_to_snapshot(&usage)))
}

fn resolve_token() -> Option<String> {
    if let Ok(v) = std::env::var("KIMI_AUTH_TOKEN") {
        let t = extract_token(&v);
        if !t.is_empty() {
            return Some(t);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.kimi_auth_token)
        .map(|s| extract_token(&s))
        .filter(|s| !s.is_empty())
}

/// If the value is a Cookie header containing `kimi-auth=…`, return that value;
/// otherwise return the (trimmed) input as a bare token.
fn extract_token(raw: &str) -> String {
    for part in raw.split(';') {
        if let Some(v) = part.trim().strip_prefix("kimi-auth=") {
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    raw.trim().to_string()
}

async fn fetch_usages(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .post(USAGES_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Cookie", format!("kimi-auth={token}"))
        .header("Content-Type", "application/json")
        .header("Origin", "https://www.kimi.com")
        .header("Referer", "https://www.kimi.com/code/console")
        .header("connect-protocol-version", "1")
        .header("x-language", "en-US")
        .header("x-msh-platform", "web")
        .body(r#"{"scope":["FEATURE_CODING"]}"#)
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

fn map_to_snapshot(u: &Usage) -> QuotaSnapshot {
    let mut tiers: Vec<TierEntry> = Vec::new();

    // Weekly window (top-level detail).
    let weekly_limit = u.detail.as_ref().and_then(Detail::limit_i);
    let weekly_used = u.detail.as_ref().and_then(Detail::used_i);
    let weekly_reset = u.detail.as_ref().and_then(|d| d.reset_time.clone());
    // remaining field, else limit − used.
    let weekly_remaining = u.detail.as_ref().and_then(Detail::remaining_i).or_else(|| {
        match (weekly_limit, weekly_used) {
            (Some(l), Some(us)) => Some((l - us).max(0)),
            _ => None,
        }
    });

    if let Some(limit) = weekly_limit {
        if limit > 0 {
            tiers.push(TierEntry {
                name: "Weekly".to_string(),
                quota: limit,
                remaining: weekly_remaining.unwrap_or(0).max(0),
                reset_time: weekly_reset.clone(),
            });
        }
    }

    // 5-hour rate-limit window (first per-window limit).
    if let Some(ld) = u.limits.first().and_then(|l| l.detail.as_ref()) {
        if let Some(total) = ld.limit_i() {
            if total > 0 {
                let used = ld.used_i().unwrap_or(0);
                tiers.push(TierEntry {
                    name: "5h Rate Limit".to_string(),
                    quota: total,
                    remaining: (total - used).max(0),
                    reset_time: ld.reset_time.clone(),
                });
            }
        }
    }

    QuotaSnapshot {
        // Mac uses a nil plan_type here.
        plan_type: String::new(),
        remaining: weekly_remaining.unwrap_or(0).max(0),
        quota: weekly_limit.unwrap_or(0).max(0),
        session_reset: weekly_reset,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_usage(json: &str) -> Usage {
        serde_json::from_str::<UsagesResponse>(json)
            .unwrap()
            .usages
            .into_iter()
            .next()
            .unwrap()
    }

    const SAMPLE: &str = r#"{"usages":[{
        "detail":{"limit":"1000","used":"300","remaining":"700","resetTime":"2026-08-01T00:00:00Z"},
        "limits":[{"detail":{"limit":"50","used":"10","resetTime":"2026-07-06T05:00:00Z"}}]
    }]}"#;

    #[test]
    fn maps_weekly_and_rate_limit_windows() {
        let snap = map_to_snapshot(&first_usage(SAMPLE));
        assert_eq!(snap.quota, 1000);
        assert_eq!(snap.remaining, 700);
        assert_eq!(snap.session_reset.as_deref(), Some("2026-08-01T00:00:00Z"));
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "Weekly");
        assert_eq!(snap.tiers[0].quota, 1000);
        assert_eq!(snap.tiers[0].remaining, 700);
        assert_eq!(snap.tiers[1].name, "5h Rate Limit");
        assert_eq!(snap.tiers[1].quota, 50);
        assert_eq!(snap.tiers[1].remaining, 40); // 50 - 10
    }

    #[test]
    fn weekly_remaining_falls_back_to_limit_minus_used() {
        let u = first_usage(
            r#"{"usages":[{"detail":{"limit":"100","used":"30"}}]}"#, // no "remaining"
        );
        let snap = map_to_snapshot(&u);
        assert_eq!(snap.remaining, 70);
        assert_eq!(snap.tiers[0].remaining, 70);
    }

    #[test]
    fn no_limits_yields_only_weekly_tier() {
        let u = first_usage(
            r#"{"usages":[{"detail":{"limit":"500","used":"100","remaining":"400"}}]}"#,
        );
        let snap = map_to_snapshot(&u);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Weekly");
    }

    #[test]
    fn non_numeric_strings_are_ignored() {
        // Garbage limit → no weekly tier, zero gauge.
        let u = first_usage(r#"{"usages":[{"detail":{"limit":"oops","used":"x"}}]}"#);
        let snap = map_to_snapshot(&u);
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
    }

    #[test]
    fn rate_limit_remaining_floors_at_zero() {
        let u = first_usage(
            r#"{"usages":[{"detail":{"limit":"100","used":"0","remaining":"100"},"limits":[{"detail":{"limit":"20","used":"50"}}]}]}"#,
        );
        let snap = map_to_snapshot(&u);
        let rl = snap
            .tiers
            .iter()
            .find(|t| t.name == "5h Rate Limit")
            .unwrap();
        assert_eq!(rl.remaining, 0); // max(0, 20 - 50)
    }

    #[test]
    fn extract_token_from_cookie_or_bare() {
        assert_eq!(extract_token("kimi-auth=eyJabc; other=x"), "eyJabc");
        assert_eq!(extract_token("  eyJbareToken  "), "eyJbareToken");
        assert_eq!(extract_token("session=foo; kimi-auth=T2"), "T2");
    }
}
