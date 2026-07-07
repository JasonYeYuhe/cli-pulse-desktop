//! Perplexity credit collection — port of macOS `PerplexityCollector`
//! (itself derived from steipete/CodexBar, MIT).
//!
//! Endpoint: `GET https://www.perplexity.ai/rest/billing/credits?version=2.18&source=default`
//! Auth: **cookie-session**. `Cookie: <session>` from env
//! `PERPLEXITY_SESSION_TOKEN` / `PERPLEXITY_COOKIE`, or the Settings-stored
//! `perplexity_cookie`. Browser-like `Origin`/`Referer`/`User-Agent` headers are
//! sent (the endpoint 403s without them). Manual paste only (no browser import).
//!
//! Credit grants are attributed with a **waterfall** (recurring → purchased →
//! promotional): `total_usage_cents` is drained against each pool in order, so
//! each tier's `remaining = pool_total − pool_used`. Expired promotional grants
//! are excluded. Plan is inferred from the recurring pool size (Free/Pro/Max).
//!
//! Scale: **$1 = 100_000 units** (cents ÷100 ×100_000 — the OpenRouter
//! convention the Mac uses here), matching the Mac Perplexity collector so a
//! dual-writer converges.

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const CREDITS_URL: &str =
    "https://www.perplexity.ai/rest/billing/credits?version=2.18&source=default";
const UNIT_SCALE: f64 = 100_000.0;
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct CreditsResponse {
    #[serde(default, rename = "balance_cents")]
    balance_cents: f64,
    #[serde(default, rename = "renewal_date_ts")]
    renewal_date_ts: f64,
    #[serde(default, rename = "current_period_purchased_cents")]
    current_period_purchased_cents: f64,
    #[serde(default, rename = "credit_grants")]
    credit_grants: Vec<Grant>,
    #[serde(default, rename = "total_usage_cents")]
    total_usage_cents: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Grant {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default, rename = "amount_cents")]
    amount_cents: f64,
    #[serde(default, rename = "expires_at_ts")]
    expires_at_ts: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
struct Attributed {
    recurring_total: f64,
    recurring_used: f64,
    promo_total: f64,
    promo_used: f64,
    purchased_total: f64,
    purchased_used: f64,
    balance_cents: f64,
    renewal_ts: f64,
}

impl Attributed {
    /// Free = 0 (→ None), Pro = small recurring pool (< $50), Max = larger.
    fn plan_name(&self) -> Option<String> {
        if self.recurring_total <= 0.0 {
            None
        } else if self.recurring_total < 5000.0 {
            Some("Pro".to_string())
        } else {
            Some("Max".to_string())
        }
    }
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let cookie = match resolve_cookie() {
        Some(c) => c,
        None => {
            log::debug!("[Perplexity] no session cookie (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_credits(&cookie).await?;
    let resp: CreditsResponse =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    let now_ts = chrono::Utc::now().timestamp() as f64;
    Ok(Some(map_to_snapshot(&attribute(&resp, now_ts))))
}

fn resolve_cookie() -> Option<String> {
    for env in ["PERPLEXITY_SESSION_TOKEN", "PERPLEXITY_COOKIE"] {
        if let Ok(c) = std::env::var(env) {
            let c = c.trim().to_string();
            if !c.is_empty() {
                return Some(c);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.perplexity_cookie)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_credits(cookie: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(CREDITS_URL)
        .header("Cookie", cookie)
        .header("Accept", "application/json")
        .header("Origin", "https://www.perplexity.ai")
        .header("Referer", "https://www.perplexity.ai/account/usage")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/124.0 Safari/537.36",
        )
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

/// Waterfall attribution of `total_usage_cents` against the recurring →
/// purchased → promotional pools. `now_ts` (unix seconds) filters expired
/// promotional grants.
fn attribute(r: &CreditsResponse, now_ts: f64) -> Attributed {
    let recurring_total = r
        .credit_grants
        .iter()
        .filter(|g| g.kind == "recurring")
        .map(|g| g.amount_cents)
        .sum::<f64>()
        .max(0.0);
    let promo_total = r
        .credit_grants
        .iter()
        .filter(|g| g.kind == "promotional" && g.expires_at_ts.unwrap_or(f64::INFINITY) > now_ts)
        .map(|g| g.amount_cents)
        .sum::<f64>()
        .max(0.0);
    let purchased_from_grants = r
        .credit_grants
        .iter()
        .filter(|g| g.kind == "purchased")
        .map(|g| g.amount_cents)
        .sum::<f64>()
        .max(0.0);
    // Purchased may appear in the top-level field, the grants, or both — take
    // the larger to avoid double-counting (verbatim from upstream).
    let purchased_total = purchased_from_grants.max(r.current_period_purchased_cents.max(0.0));

    let mut remaining = r.total_usage_cents.max(0.0);
    let recurring_used = remaining.min(recurring_total);
    remaining -= recurring_used;
    let purchased_used = remaining.min(purchased_total);
    remaining -= purchased_used;
    let promo_used = remaining.min(promo_total);

    Attributed {
        recurring_total,
        recurring_used,
        promo_total,
        promo_used,
        purchased_total,
        purchased_used,
        balance_cents: r.balance_cents,
        renewal_ts: r.renewal_date_ts,
    }
}

/// cents → integer units at $1 = 100_000 units. Truncates toward zero (matching
/// the Mac's `Int(...)`), floored at 0, saturating at `i64::MAX`.
fn units_from_cents(cents: f64) -> i64 {
    if !cents.is_finite() || cents <= 0.0 {
        return 0;
    }
    let u = (cents / 100.0) * UNIT_SCALE;
    if u >= i64::MAX as f64 {
        i64::MAX
    } else {
        u as i64
    }
}

fn ts_to_rfc3339(secs: f64) -> Option<String> {
    if !secs.is_finite() || secs <= 0.0 {
        return None;
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0).map(|d| d.to_rfc3339())
}

fn map_to_snapshot(a: &Attributed) -> QuotaSnapshot {
    let reset = ts_to_rfc3339(a.renewal_ts);
    let mut tiers: Vec<TierEntry> = Vec::new();
    if a.recurring_total > 0.0 {
        tiers.push(TierEntry {
            name: "Recurring".to_string(),
            quota: units_from_cents(a.recurring_total),
            remaining: units_from_cents(a.recurring_total - a.recurring_used),
            reset_time: reset.clone(),
        });
    }
    if a.promo_total > 0.0 {
        tiers.push(TierEntry {
            name: "Bonus".to_string(),
            quota: units_from_cents(a.promo_total),
            remaining: units_from_cents(a.promo_total - a.promo_used),
            reset_time: None,
        });
    }
    if a.purchased_total > 0.0 {
        tiers.push(TierEntry {
            name: "Purchased".to_string(),
            quota: units_from_cents(a.purchased_total),
            remaining: units_from_cents(a.purchased_total - a.purchased_used),
            reset_time: None,
        });
    }

    let total_cents = a.recurring_total + a.promo_total + a.purchased_total;
    QuotaSnapshot {
        status_text: None,
        plan_type: a.plan_name().unwrap_or_else(|| "Unknown".to_string()),
        remaining: units_from_cents(a.balance_cents),
        quota: units_from_cents(total_cents),
        session_reset: reset,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_800_000_000.0; // fixed "now" for promo-expiry tests

    fn resp(json: &str) -> CreditsResponse {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn units_scale_dollar_is_100k() {
        assert_eq!(units_from_cents(100.0), 100_000); // $1.00
        assert_eq!(units_from_cents(5000.0), 5_000_000); // $50
        assert_eq!(units_from_cents(0.0), 0);
        assert_eq!(units_from_cents(-5.0), 0);
    }

    #[test]
    fn waterfall_drains_recurring_then_purchased_then_promo() {
        let r = resp(
            r#"{
                "balance_cents": 4500,
                "renewal_date_ts": 1800000000,
                "total_usage_cents": 6000,
                "credit_grants": [
                    {"type":"recurring","amount_cents":5000},
                    {"type":"purchased","amount_cents":2000},
                    {"type":"promotional","amount_cents":1000}
                ]
            }"#,
        );
        let a = attribute(&r, NOW);
        // usage 6000 → 5000 from recurring, 1000 from purchased, 0 from promo.
        assert_eq!(a.recurring_used, 5000.0);
        assert_eq!(a.purchased_used, 1000.0);
        assert_eq!(a.promo_used, 0.0);
        let snap = map_to_snapshot(&a);
        assert_eq!(snap.tiers.len(), 3);
        assert_eq!(snap.tiers[0].name, "Recurring");
        assert_eq!(snap.tiers[0].quota, 5_000_000); // $50
        assert_eq!(snap.tiers[0].remaining, 0); // fully used
        assert_eq!(snap.tiers[1].name, "Bonus"); // promo → "Bonus"
        assert_eq!(snap.tiers[1].remaining, 1_000_000); // $10 unused
        assert_eq!(snap.tiers[2].name, "Purchased");
        assert_eq!(snap.tiers[2].remaining, 1_000_000); // $20 - $10 used = $10
                                                        // Top-level: remaining = balance ($45), quota = all pools ($80).
        assert_eq!(snap.remaining, 4_500_000);
        assert_eq!(snap.quota, 8_000_000);
        assert_eq!(snap.plan_type, "Max"); // recurring 5000 → not < 5000
    }

    #[test]
    fn expired_promo_is_excluded() {
        let json = |exp: f64| {
            format!(
                r#"{{"total_usage_cents":0,"credit_grants":[{{"type":"promotional","amount_cents":1000,"expires_at_ts":{exp}}}]}}"#
            )
        };
        // expires in the past → excluded.
        let a = attribute(&resp(&json(NOW - 100.0)), NOW);
        assert_eq!(a.promo_total, 0.0);
        // expires in the future → included.
        let a = attribute(&resp(&json(NOW + 100.0)), NOW);
        assert_eq!(a.promo_total, 1000.0);
    }

    #[test]
    fn purchased_takes_max_of_grants_and_field() {
        // field larger than grants → field wins.
        let a = attribute(
            &resp(
                r#"{"current_period_purchased_cents":3000,"total_usage_cents":0,"credit_grants":[{"type":"purchased","amount_cents":2000}]}"#,
            ),
            NOW,
        );
        assert_eq!(a.purchased_total, 3000.0);
        // grants larger → grants win.
        let a = attribute(
            &resp(
                r#"{"current_period_purchased_cents":1000,"total_usage_cents":0,"credit_grants":[{"type":"purchased","amount_cents":4000}]}"#,
            ),
            NOW,
        );
        assert_eq!(a.purchased_total, 4000.0);
    }

    #[test]
    fn plan_inference_free_pro_max() {
        let mk = |rec: f64| Attributed {
            recurring_total: rec,
            recurring_used: 0.0,
            promo_total: 0.0,
            promo_used: 0.0,
            purchased_total: 0.0,
            purchased_used: 0.0,
            balance_cents: 0.0,
            renewal_ts: 0.0,
        };
        assert_eq!(mk(0.0).plan_name(), None);
        assert_eq!(mk(3000.0).plan_name().as_deref(), Some("Pro"));
        assert_eq!(mk(9000.0).plan_name().as_deref(), Some("Max"));
        // Free → "Unknown" at the snapshot level.
        assert_eq!(map_to_snapshot(&mk(0.0)).plan_type, "Unknown");
    }

    #[test]
    fn renewal_ts_to_rfc3339_and_zero_is_none() {
        let a = attribute(
            &resp(r#"{"renewal_date_ts":1800000000,"total_usage_cents":0,"credit_grants":[]}"#),
            NOW,
        );
        assert!(map_to_snapshot(&a).session_reset.is_some());
        let a = attribute(&resp(r#"{"total_usage_cents":0,"credit_grants":[]}"#), NOW);
        assert!(map_to_snapshot(&a).session_reset.is_none()); // ts 0 → None
    }
}
