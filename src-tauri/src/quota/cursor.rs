//! Cursor quota collection — port of macOS
//! `Collectors/CursorCollector.swift`.
//!
//! Auth source: env `CURSOR_COOKIE` only in v0.4.3. Settings UI for
//! cookie entry deferred to v0.4.4 — Gemini 3.1 Pro 2026-05-02 review
//! flagged this as a UX gap; backend ships value when env is set.
//!
//! Endpoint: `GET https://cursor.com/api/usage-summary` with
//! `Cookie: <CURSOR_COOKIE>`. 15s timeout.
//!
//! Tiers emitted: "Plan" (cents-scaled quota/remaining + billing-cycle
//! reset), "On-Demand" (when `onDemand.limit > 0`).

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const USAGE_URL: &str = "https://cursor.com/api/usage-summary";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct UsageResponse {
    #[serde(rename = "membershipType", default)]
    membership_type: Option<String>,
    #[serde(rename = "billingCycleEnd", default)]
    billing_cycle_end: Option<String>,
    #[serde(rename = "individualUsage", default)]
    individual_usage: IndividualUsage,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct IndividualUsage {
    #[serde(default)]
    plan: PlanUsage,
    #[serde(rename = "onDemand", default)]
    on_demand: OnDemandUsage,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PlanUsage {
    #[serde(default)]
    used: i64,
    #[serde(default)]
    limit: i64,
    #[serde(default)]
    remaining: Option<i64>,
    // `totalPercentUsed` is returned by Cursor's API but the desktop
    // computes percentage from used/limit instead — serde silently
    // ignores it on deserialize.
}

#[derive(Debug, Clone, Default, Deserialize)]
struct OnDemandUsage {
    #[serde(default)]
    used: i64,
    #[serde(default)]
    limit: Option<i64>,
}

/// Collect Cursor quota. v0.4.6 — credential read priority:
///   1. Env `CURSOR_COOKIE` (backwards compat for power users)
///   2. `provider_creds.json` `cursor_cookie` field (Settings UI)
///   3. None → silent debug skip.
///
/// v0.4.20 return shape:
/// - `Ok(Some(snap))` — success.
/// - `Ok(None)` — no cookie configured (env unset + Settings empty).
/// - `Err(...)` — HTTP failure (auth, rate limit, network).
pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let cookie = std::env::var("CURSOR_COOKIE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            crate::provider_creds::load()
                .ok()
                .and_then(|c| c.cursor_cookie)
                .filter(|s| !s.is_empty())
        });
    let cookie = match cookie {
        Some(c) => c,
        None => {
            log::debug!("[Cursor] no credential (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    match fetch_usage(&cookie).await {
        Ok(usage) => Ok(Some(map_to_snapshot(&usage))),
        Err(e) => {
            log::warn!("[Cursor] /usage-summary fetch failed (non-fatal): {e}");
            Err(CollectorError::Http(format!("/usage-summary: {e}")))
        }
    }
}

async fn fetch_usage(cookie: &str) -> Result<UsageResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let resp = client
        .get(USAGE_URL)
        .header("Cookie", cookie)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(120).collect();
        return Err(format!("HTTP {} — {}", status.as_u16(), snippet));
    }
    resp.json::<UsageResponse>()
        .await
        .map_err(|e| format!("parse: {e}"))
}

fn map_to_snapshot(usage: &UsageResponse) -> QuotaSnapshot {
    let plan = &usage.individual_usage.plan;
    let on_demand = &usage.individual_usage.on_demand;
    let mut tiers = Vec::new();

    if plan.limit > 0 {
        // remaining = explicit field if present, else limit - used.
        let remaining = plan.remaining.unwrap_or(plan.limit - plan.used);
        tiers.push(TierEntry {
            name: "Plan".to_string(),
            quota: plan.limit,
            remaining: remaining.max(0),
            reset_time: usage.billing_cycle_end.clone(),
        });
    }
    if let Some(limit) = on_demand.limit {
        if limit > 0 {
            tiers.push(TierEntry {
                name: "On-Demand".to_string(),
                quota: limit,
                remaining: (limit - on_demand.used).max(0),
                reset_time: usage.billing_cycle_end.clone(),
            });
        }
    }

    let plan_type = match usage.membership_type.as_deref() {
        Some(s) if !s.is_empty() => capitalize(s),
        _ => "Unknown".to_string(),
    };

    let outer_remaining = if plan.limit > 0 {
        plan.remaining.unwrap_or(plan.limit - plan.used).max(0)
    } else {
        0
    };

    QuotaSnapshot {
        plan_type,
        remaining: outer_remaining,
        quota: plan.limit,
        session_reset: usage.billing_cycle_end.clone(),
        tiers,
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_full_with_on_demand() {
        let json = r#"{
            "membershipType": "pro",
            "billingCycleEnd": "2026-05-31T00:00:00Z",
            "individualUsage": {
                "plan": {"used": 1000, "limit": 5000, "remaining": 4000, "totalPercentUsed": 20.0},
                "onDemand": {"used": 200, "limit": 1000}
            }
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "Plan");
        assert_eq!(snap.tiers[0].quota, 5000);
        assert_eq!(snap.tiers[0].remaining, 4000);
        assert_eq!(snap.tiers[1].name, "On-Demand");
        assert_eq!(snap.tiers[1].remaining, 800);
        assert_eq!(snap.plan_type, "Pro");
        assert_eq!(snap.session_reset.as_deref(), Some("2026-05-31T00:00:00Z"));
    }

    #[test]
    fn parse_response_no_on_demand() {
        let json = r#"{
            "membershipType": "free",
            "individualUsage": {
                "plan": {"used": 100, "limit": 200},
                "onDemand": {"used": 0, "limit": null}
            }
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].remaining, 100); // 200 - 100
        assert_eq!(snap.plan_type, "Free");
    }

    #[test]
    fn parse_response_remaining_falls_back_to_limit_minus_used() {
        // When `remaining` field absent, derive from limit - used.
        let json = r#"{
            "membershipType": "business",
            "individualUsage": {
                "plan": {"used": 750, "limit": 1000},
                "onDemand": {"used": 0}
            }
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers[0].remaining, 250);
    }

    #[test]
    fn parse_response_membership_unknown() {
        let json =
            r#"{"individualUsage": {"plan": {"used": 0, "limit": 100}, "onDemand": {"used": 0}}}"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.plan_type, "Unknown");
    }

    #[test]
    fn parse_response_remaining_clamps_to_zero() {
        // Defensive: used > limit (shouldn't happen but...)
        let json = r#"{
            "membershipType": "pro",
            "individualUsage": {"plan": {"used": 200, "limit": 100}, "onDemand": {"used": 0}}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers[0].remaining, 0);
    }
}
