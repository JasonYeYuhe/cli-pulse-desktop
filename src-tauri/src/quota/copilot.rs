//! GitHub Copilot quota collection — port of macOS
//! `Collectors/CopilotCollector.swift`.
//!
//! Auth source: env `COPILOT_API_TOKEN` only in v0.4.3. UI for token
//! entry deferred to v0.4.4 (Gemini 3.1 Pro 2026-05-02 review).
//!
//! Endpoint: `GET https://api.github.com/copilot_internal/user`.
//! Headers: `Authorization: token <token>` (NOT "Bearer"), the GitHub
//! Copilot internal API quirk requires `Editor-Version`,
//! `Editor-Plugin-Version`, `User-Agent` to be set verbatim or it
//! 401s. Mirror Mac line 38-40.
//!
//! Tiers emitted: "Premium", "Chat" (each with `entitlement` quota
//! and `remaining` or percent-derived remaining).

use std::time::Duration;

use serde::Deserialize;

use super::{QuotaSnapshot, TierEntry};

const USAGE_URL: &str = "https://api.github.com/copilot_internal/user";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct UsageResponse {
    #[serde(rename = "copilotPlan", alias = "copilot_plan", default)]
    plan: Option<String>,
    #[serde(rename = "quotaResetDate", alias = "quota_reset_date", default)]
    reset_date: Option<String>,
    #[serde(rename = "quotaSnapshots", alias = "quota_snapshots", default)]
    snapshots: Option<Snapshots>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Snapshots {
    #[serde(rename = "premiumInteractions", default)]
    premium_interactions: Option<TierSnapshot>,
    #[serde(default)]
    chat: Option<TierSnapshot>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TierSnapshot {
    #[serde(default)]
    entitlement: Option<f64>,
    #[serde(default)]
    remaining: Option<f64>,
    #[serde(rename = "percentRemaining", alias = "percent_remaining", default)]
    percent_remaining: Option<f64>,
}

/// Collect Copilot quota. v0.4.6 — credential read priority:
///   1. Env `COPILOT_API_TOKEN` (backwards compat for power users)
///   2. `provider_creds.json` `copilot_token` field (Settings UI)
///   3. None → silent debug skip.
pub async fn collect() -> Option<QuotaSnapshot> {
    let token = std::env::var("COPILOT_API_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            crate::provider_creds::load()
                .ok()
                .and_then(|c| c.copilot_token)
                .filter(|s| !s.is_empty())
        });
    let token = match token {
        Some(t) => t,
        None => {
            log::debug!("[Copilot] no credential (env or Settings UI) — skipping");
            return None;
        }
    };
    match fetch_usage(&token).await {
        Ok(usage) => Some(map_to_snapshot(&usage)),
        Err(e) => {
            log::warn!("[Copilot] /copilot_internal/user fetch failed (non-fatal): {e}");
            None
        }
    }
}

async fn fetch_usage(token: &str) -> Result<UsageResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    // GitHub Copilot internal API requires the editor headers verbatim
    // — without them the endpoint 401s. Mirror Mac line 38-40.
    let resp = client
        .get(USAGE_URL)
        .header("Authorization", format!("token {token}"))
        .header("Accept", "application/json")
        .header("Editor-Version", "vscode/1.96.2")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
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
    let mut tiers = Vec::new();
    let snapshots = usage.snapshots.clone().unwrap_or_default();

    if let Some(t) = &snapshots.premium_interactions {
        if let Some(tier) = build_tier("Premium", t, &usage.reset_date) {
            tiers.push(tier);
        }
    }
    if let Some(t) = &snapshots.chat {
        if let Some(tier) = build_tier("Chat", t, &usage.reset_date) {
            tiers.push(tier);
        }
    }

    let plan_type = match usage.plan.as_deref() {
        Some(s) if !s.is_empty() => capitalize(s),
        _ => "Unknown".to_string(),
    };

    // Outer remaining/quota: prefer Premium, else Chat, else 0.
    let outer = tiers
        .first()
        .map(|t| (t.quota, t.remaining))
        .unwrap_or((0, 0));

    QuotaSnapshot {
        plan_type,
        remaining: outer.1,
        quota: outer.0,
        session_reset: usage.reset_date.clone(),
        tiers,
    }
}

fn build_tier(name: &str, t: &TierSnapshot, reset: &Option<String>) -> Option<TierEntry> {
    let entitlement = t.entitlement.unwrap_or(0.0);
    if entitlement <= 0.0 {
        return None;
    }
    // remaining = explicit > pct-derived > full entitlement (Mac line 91-92, 96-97).
    let remaining = t
        .remaining
        .or_else(|| t.percent_remaining.map(|p| p / 100.0 * entitlement))
        .unwrap_or(entitlement);
    Some(TierEntry {
        name: name.to_string(),
        quota: entitlement.round() as i64,
        remaining: remaining.round().max(0.0) as i64,
        reset_time: reset.clone(),
    })
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
    fn parse_response_camel_case() {
        let json = r#"{
            "copilotPlan": "business",
            "quotaResetDate": "2026-05-09T00:00:00Z",
            "quotaSnapshots": {
                "premiumInteractions": {"entitlement": 300.0, "remaining": 245.0, "percentRemaining": 81.6},
                "chat": {"entitlement": 1000.0, "remaining": 850.0, "percentRemaining": 85.0}
            }
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "Premium");
        assert_eq!(snap.tiers[0].quota, 300);
        assert_eq!(snap.tiers[0].remaining, 245);
        assert_eq!(snap.tiers[1].name, "Chat");
        assert_eq!(snap.tiers[1].remaining, 850);
        assert_eq!(snap.plan_type, "Business");
        assert_eq!(snap.session_reset.as_deref(), Some("2026-05-09T00:00:00Z"));
    }

    #[test]
    fn parse_response_snake_case() {
        let json = r#"{
            "copilot_plan": "individual",
            "quota_reset_date": "2026-06-01T00:00:00Z",
            "quota_snapshots": {
                "premiumInteractions": {"entitlement": 500.0, "remaining": 400.0}
            }
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.plan_type, "Individual");
    }

    #[test]
    fn parse_response_chat_only_no_premium() {
        // Premium entitlement = 0 → skip; Chat present → emit.
        let json = r#"{
            "copilotPlan": "free",
            "quotaSnapshots": {
                "premiumInteractions": {"entitlement": 0},
                "chat": {"entitlement": 100.0, "remaining": 50.0}
            }
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Chat");
    }

    #[test]
    fn tier_remaining_falls_back_to_pct() {
        // remaining absent → use percentRemaining/100 * entitlement.
        let json = r#"{
            "copilotPlan": "business",
            "quotaSnapshots": {
                "premiumInteractions": {"entitlement": 300.0, "percentRemaining": 50.0}
            }
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers[0].remaining, 150);
    }

    #[test]
    fn tier_remaining_falls_back_to_entitlement() {
        // Both remaining and percentRemaining absent → assume full.
        let json = r#"{
            "copilotPlan": "business",
            "quotaSnapshots": {
                "premiumInteractions": {"entitlement": 300.0}
            }
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers[0].remaining, 300);
    }

    #[test]
    fn plan_type_unknown_when_field_missing() {
        let json = r#"{}"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.plan_type, "Unknown");
        assert_eq!(snap.tiers.len(), 0);
    }
}
