//! Augment credit collection — port of macOS `AugmentCollector`.
//!
//! Endpoints: `GET https://app.augmentcode.com/api/credits` (required) and
//! `GET https://app.augmentcode.com/api/subscription` (optional — plan name +
//! billing-period end).
//! Auth: **cookie-session** (not api-key). `Cookie: <session>` from env
//! `AUGMENT_COOKIE` or the Settings-stored `augment_cookie`.
//!
//! Unlike the Mac's automatic browser-cookie import, the desktop uses a manual
//! paste (same as the Cursor collector) — browser cookie extraction isn't
//! wired here. Cookies expire, so this collector will error (red badge) once
//! the session lapses; re-paste to refresh.
//!
//! Real depleting quota: `quota = usageUnitsRemaining +
//! usageUnitsConsumedThisBillingCycle` (the cap), `remaining =
//! usageUnitsRemaining`. Raw integer unit counts (no scaling). The optional
//! subscription supplies the plan name and the billing-cycle reset.

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const CREDITS_URL: &str = "https://app.augmentcode.com/api/credits";
const SUBSCRIPTION_URL: &str = "https://app.augmentcode.com/api/subscription";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct CreditsResponse {
    #[serde(default, rename = "usageUnitsRemaining")]
    remaining: i64,
    #[serde(default, rename = "usageUnitsConsumedThisBillingCycle")]
    consumed: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SubscriptionResponse {
    #[serde(default, rename = "planName")]
    plan_name: Option<String>,
    #[serde(default, rename = "billingPeriodEnd")]
    billing_period_end: Option<String>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let cookie = match resolve_cookie() {
        Some(c) => c,
        None => {
            log::debug!("[Augment] no session cookie (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;

    let credits_body = fetch(&client, CREDITS_URL, &cookie).await?;
    let credits: CreditsResponse = serde_json::from_str(&credits_body)
        .map_err(|e| CollectorError::Http(format!("credits parse: {e}")))?;

    // Subscription is best-effort — a failure (or absent plan) must not sink
    // the whole collector, matching the Mac's `try?`.
    let subscription = match fetch(&client, SUBSCRIPTION_URL, &cookie).await {
        Ok(body) => serde_json::from_str::<SubscriptionResponse>(&body).ok(),
        Err(_) => None,
    };

    Ok(Some(map_to_snapshot(&credits, subscription.as_ref())))
}

fn resolve_cookie() -> Option<String> {
    if let Ok(c) = std::env::var("AUGMENT_COOKIE") {
        let c = c.trim().to_string();
        if !c.is_empty() {
            return Some(c);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.augment_cookie)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    cookie: &str,
) -> Result<String, CollectorError> {
    let resp = client
        .get(url)
        .header("Cookie", cookie)
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

fn map_to_snapshot(c: &CreditsResponse, sub: Option<&SubscriptionResponse>) -> QuotaSnapshot {
    let remaining = c.remaining.max(0);
    let consumed = c.consumed.max(0);
    let total = remaining + consumed;

    let reset = sub
        .and_then(|s| s.billing_period_end.clone())
        .filter(|s| !s.is_empty());
    let plan = sub
        .and_then(|s| s.plan_name.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();

    let mut tiers: Vec<TierEntry> = Vec::new();
    if total > 0 {
        tiers.push(TierEntry {
            name: "Credits".to_string(),
            quota: total,
            remaining,
            reset_time: reset.clone(),
        });
    }

    QuotaSnapshot {
        plan_type: plan,
        remaining,
        quota: total,
        session_reset: reset,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credits(remaining: i64, consumed: i64) -> CreditsResponse {
        CreditsResponse {
            remaining,
            consumed,
        }
    }

    #[test]
    fn parses_credits_response_camelcase() {
        let c: CreditsResponse = serde_json::from_str(
            r#"{"usageUnitsRemaining":300,"usageUnitsConsumedThisBillingCycle":700,"usageUnitsAvailable":1000}"#,
        )
        .unwrap();
        assert_eq!(c.remaining, 300);
        assert_eq!(c.consumed, 700);
    }

    #[test]
    fn maps_credits_to_depleting_tier() {
        let snap = map_to_snapshot(&credits(300, 700), None);
        assert_eq!(snap.quota, 1000); // 300 + 700
        assert_eq!(snap.remaining, 300);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Credits");
        assert_eq!(snap.tiers[0].quota, 1000);
        assert_eq!(snap.tiers[0].remaining, 300);
        // No subscription → empty plan, no reset.
        assert_eq!(snap.plan_type, "");
        assert!(snap.session_reset.is_none());
        assert!(snap.tiers[0].reset_time.is_none());
    }

    #[test]
    fn subscription_supplies_plan_and_reset() {
        let sub = SubscriptionResponse {
            plan_name: Some("Pro".to_string()),
            billing_period_end: Some("2026-08-01T00:00:00Z".to_string()),
        };
        let snap = map_to_snapshot(&credits(50, 50), Some(&sub));
        assert_eq!(snap.plan_type, "Pro");
        assert_eq!(snap.session_reset.as_deref(), Some("2026-08-01T00:00:00Z"));
        assert_eq!(
            snap.tiers[0].reset_time.as_deref(),
            Some("2026-08-01T00:00:00Z")
        );
    }

    #[test]
    fn empty_subscription_fields_are_ignored() {
        let sub = SubscriptionResponse {
            plan_name: Some(String::new()),
            billing_period_end: Some(String::new()),
        };
        let snap = map_to_snapshot(&credits(10, 0), Some(&sub));
        assert_eq!(snap.plan_type, "");
        assert!(snap.session_reset.is_none());
    }

    #[test]
    fn zero_total_and_negative_floor() {
        let snap = map_to_snapshot(&credits(0, 0), None);
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        let snap = map_to_snapshot(&credits(-9, -3), None);
        assert_eq!(snap.quota, 0);
        assert_eq!(snap.remaining, 0);
    }
}
