//! Codebuff credit balance + (best-effort) subscription — port of macOS
//! `CodebuffCollector` (itself derived from steipete/CodexBar, MIT).
//!
//! Primary (required): `POST https://www.codebuff.com/api/v1/usage` (body
//! `{"fingerprintId":"clipulse-usage"}`) returns credits used/total/remaining
//! and the next reset. Secondary (best-effort, ~2s grace):
//! `GET /api/user/subscription` adds a weekly window and tier. Auth:
//! `Authorization: Bearer <key>` from env `CODEBUFF_API_KEY` or the Settings
//! `codebuff_api_key`.
//!
//! Codebuff credits have a hard cap + reset, so a positive total maps to a real
//! `.quota` (a "Credits" gauge). A degenerate `total <= 0` (bare balance, no
//! usable cap) falls back to status-only — a 0/healthy gauge would mislead. The
//! subscription call is bounded to a 2-second per-request timeout so a slow
//! secondary can never stall the required usage render (mirrors the Mac's grace
//! window); its weekly window enriches the result only if it returns in time.

use std::time::Duration;

use serde_json::Value;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const USAGE_URL: &str = "https://www.codebuff.com/api/v1/usage";
const SUBSCRIPTION_URL: &str = "https://www.codebuff.com/api/user/subscription";
const TIMEOUT: Duration = Duration::from_secs(15);
const SUBSCRIPTION_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default)]
struct UsagePayload {
    used: Option<f64>,
    total: Option<f64>,
    remaining: Option<f64>,
    next_quota_reset: Option<String>,
    auto_topup: Option<bool>,
}

#[derive(Debug, Clone, Default)]
struct SubscriptionPayload {
    tier: Option<String>,
    weekly_used: Option<f64>,
    weekly_limit: Option<f64>,
    weekly_resets_at: Option<String>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_token() {
        Some(t) => t,
        None => {
            log::debug!("[Codebuff] no API key (env or Settings) — skipping");
            return Ok(None);
        }
    };
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;

    // Usage is required; subscription is best-effort and bounded to a 2s
    // per-request timeout so a slow secondary can't stall the usage render.
    let (usage_res, sub_res) = tokio::join!(
        fetch_usage(&client, &token),
        fetch_subscription(&client, &token),
    );
    let usage = usage_res?;
    let subscription = sub_res.ok();
    Ok(Some(build_snapshot(&usage, subscription.as_ref())))
}

fn resolve_token() -> Option<String> {
    if let Ok(k) = std::env::var("CODEBUFF_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.codebuff_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_usage(
    client: &reqwest::Client,
    token: &str,
) -> Result<UsagePayload, CollectorError> {
    let resp = client
        .post(USAGE_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&serde_json::json!({ "fingerprintId": "clipulse-usage" }))
        .send()
        .await
        .map_err(|e| CollectorError::Http(format!("usage request: {e}")))?;
    if !resp.status().is_success() {
        return Err(CollectorError::Http(format!(
            "usage HTTP {}",
            resp.status().as_u16()
        )));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| CollectorError::Http(format!("usage body: {e}")))?;
    parse_usage(&body)
}

async fn fetch_subscription(
    client: &reqwest::Client,
    token: &str,
) -> Result<SubscriptionPayload, CollectorError> {
    let resp = client
        .get(SUBSCRIPTION_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .timeout(SUBSCRIPTION_GRACE)
        .send()
        .await
        .map_err(|e| CollectorError::Http(format!("subscription request: {e}")))?;
    if !resp.status().is_success() {
        return Err(CollectorError::Http(format!(
            "subscription HTTP {}",
            resp.status().as_u16()
        )));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| CollectorError::Http(format!("subscription body: {e}")))?;
    parse_subscription(&body)
}

/// Number tolerant of JSON number or numeric-string encodings (finite only).
fn num(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

fn string(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// ISO8601 (fractional or plain) | epoch seconds | epoch millis → RFC3339.
fn date_iso(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                return None;
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(t) {
                return Some(dt.with_timezone(&chrono::Utc).to_rfc3339());
            }
            t.parse::<f64>().ok().and_then(epoch_iso)
        }
        Some(Value::Number(n)) => n.as_f64().and_then(epoch_iso),
        _ => None,
    }
}

fn epoch_iso(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    // Heuristic: > ~year-2286-in-seconds ⇒ the value is milliseconds.
    let secs = if value > 10_000_000_000.0 {
        (value / 1000.0) as i64
    } else {
        value as i64
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0).map(|d| d.to_rfc3339())
}

fn parse_usage(body: &str) -> Result<UsagePayload, CollectorError> {
    let root: Value = serde_json::from_str(body)
        .map_err(|e| CollectorError::SchemaOrIo(format!("usage parse: {e}")))?;
    Ok(UsagePayload {
        used: num(root.get("usage")).or_else(|| num(root.get("used"))),
        total: num(root.get("quota")).or_else(|| num(root.get("limit"))),
        remaining: num(root.get("remainingBalance")).or_else(|| num(root.get("remaining"))),
        next_quota_reset: date_iso(root.get("next_quota_reset")),
        auto_topup: root
            .get("autoTopupEnabled")
            .and_then(Value::as_bool)
            .or_else(|| root.get("auto_topup_enabled").and_then(Value::as_bool)),
    })
}

fn parse_subscription(body: &str) -> Result<SubscriptionPayload, CollectorError> {
    let root: Value = serde_json::from_str(body)
        .map_err(|e| CollectorError::SchemaOrIo(format!("subscription parse: {e}")))?;
    let sub = root.get("subscription");
    let rate = root.get("rateLimit");
    let tier = string(sub.and_then(|s| s.get("displayName")))
        .or_else(|| string(root.get("displayName")))
        .or_else(|| string(sub.and_then(|s| s.get("tier"))))
        .or_else(|| string(root.get("tier")))
        .or_else(|| string(sub.and_then(|s| s.get("scheduledTier"))));
    Ok(SubscriptionPayload {
        tier,
        weekly_used: num(rate.and_then(|r| r.get("weeklyUsed")))
            .or_else(|| num(rate.and_then(|r| r.get("used")))),
        weekly_limit: num(rate.and_then(|r| r.get("weeklyLimit")))
            .or_else(|| num(rate.and_then(|r| r.get("limit")))),
        weekly_resets_at: date_iso(rate.and_then(|r| r.get("weeklyResetsAt"))),
    })
}

fn resolved_total(u: &UsagePayload) -> Option<f64> {
    if let Some(t) = u.total {
        return Some(t.max(0.0));
    }
    if let (Some(used), Some(rem)) = (u.used, u.remaining) {
        return Some((used + rem).max(0.0));
    }
    None
}

fn resolved_used(u: &UsagePayload) -> f64 {
    if let Some(used) = u.used {
        return used.max(0.0);
    }
    if let (Some(total), Some(rem)) = (resolved_total(u), u.remaining) {
        return (total - rem).max(0.0);
    }
    0.0
}

/// Mirror Swift `String.capitalized`: uppercase the first alphanumeric of each
/// word and lowercase the rest, where a word starts after ANY non-alphanumeric
/// character — space, hyphen, apostrophe, underscore, etc. (A whitespace-only
/// split would render `"pro-max"` as `"Pro-max"` instead of `"Pro-Max"`, and
/// since `plan_type` is an uploaded wire field that would diverge a dual-writer
/// row from the Mac.)
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_boundary = true;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            if at_boundary {
                out.extend(ch.to_uppercase());
            } else {
                out.extend(ch.to_lowercase());
            }
            at_boundary = false;
        } else {
            out.push(ch);
            at_boundary = true;
        }
    }
    out
}

fn group_int(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let bytes = digits.as_bytes();
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// Grouped credits: 0 decimals at ≥1000, up to 1 decimal below (trailing .0
/// trimmed). Mirrors the Mac `compactCredits`.
fn compact_credits(value: f64) -> String {
    let v = value.max(0.0);
    if v >= 1000.0 {
        group_int(v.round() as i64)
    } else {
        let r = (v * 10.0).round() / 10.0;
        if r.fract().abs() < 1e-9 {
            group_int(r as i64)
        } else {
            format!("{r:.1}")
        }
    }
}

fn append_auto_topup(text: String, enabled: Option<bool>) -> String {
    if enabled == Some(true) {
        format!("{text} · auto top-up")
    } else {
        text
    }
}

fn build_snapshot(u: &UsagePayload, sub: Option<&SubscriptionPayload>) -> QuotaSnapshot {
    let total = resolved_total(u);
    let used = resolved_used(u);
    let reset_iso = u.next_quota_reset.clone();
    let plan_type = sub
        .and_then(|s| s.tier.as_deref())
        .map(title_case)
        .unwrap_or_else(|| "API key".to_string());

    // Weekly tier from the best-effort subscription.
    let mut extra_tiers: Vec<TierEntry> = Vec::new();
    if let Some(limit) = sub.and_then(|s| s.weekly_limit) {
        if limit > 0.0 {
            let w_used = sub.and_then(|s| s.weekly_used).unwrap_or(0.0).max(0.0);
            extra_tiers.push(TierEntry {
                name: "Weekly".to_string(),
                quota: limit.round() as i64,
                remaining: (limit - w_used).round().max(0.0) as i64,
                reset_time: sub.and_then(|s| s.weekly_resets_at.clone()),
            });
        }
    }

    match total {
        Some(total) if total > 0.0 => {
            let total_int = total.round() as i64;
            let remaining_int = u
                .remaining
                .map(|r| r.round().max(0.0) as i64)
                .unwrap_or_else(|| (total_int - used.round() as i64).max(0));
            let mut tiers = vec![TierEntry {
                name: "Credits".to_string(),
                quota: total_int,
                remaining: remaining_int,
                reset_time: reset_iso.clone(),
            }];
            tiers.extend(extra_tiers);
            let status = append_auto_topup(
                format!(
                    "{} credits remaining",
                    compact_credits(remaining_int as f64)
                ),
                u.auto_topup,
            );
            QuotaSnapshot {
                status_text: Some(status),
                plan_type,
                remaining: remaining_int,
                quota: total_int,
                session_reset: reset_iso,
                tiers,
            }
        }
        _ => {
            // Degenerate: no usable cap ⇒ status-only.
            let status = if let Some(rem) = u.remaining {
                format!("{} credits remaining", compact_credits(rem))
            } else if u.used.is_some() {
                "Credits data unavailable".to_string()
            } else {
                "Connected".to_string()
            };
            QuotaSnapshot {
                status_text: Some(append_auto_topup(status, u.auto_topup)),
                plan_type,
                remaining: 0,
                quota: 0,
                session_reset: reset_iso,
                tiers: extra_tiers,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_gauge_from_total_and_remaining() {
        let u = parse_usage(
            r#"{"quota":5000,"remainingBalance":1234,"next_quota_reset":"2026-08-01T00:00:00Z","autoTopupEnabled":true}"#,
        )
        .unwrap();
        let snap = build_snapshot(&u, None);
        assert_eq!(snap.quota, 5000);
        assert_eq!(snap.remaining, 1234);
        assert_eq!(snap.plan_type, "API key");
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Credits");
        assert_eq!(
            snap.status_text.as_deref(),
            Some("1,234 credits remaining · auto top-up")
        );
        assert_eq!(
            snap.session_reset.as_deref(),
            Some("2026-08-01T00:00:00+00:00")
        );
    }

    #[test]
    fn total_derived_from_used_plus_remaining_and_string_numbers() {
        // Flexible: values as strings; no explicit total.
        let u = parse_usage(r#"{"used":"300","remaining":"700"}"#).unwrap();
        let snap = build_snapshot(&u, None);
        assert_eq!(snap.quota, 1000); // 300 + 700
        assert_eq!(snap.remaining, 700);
    }

    #[test]
    fn subscription_adds_weekly_tier_and_title_cased_plan() {
        let u = parse_usage(r#"{"quota":100,"remaining":40}"#).unwrap();
        let sub = parse_subscription(
            r#"{"subscription":{"tier":"pro max"},"rateLimit":{"weeklyLimit":50,"weeklyUsed":10,"weeklyResetsAt":"2026-07-14T00:00:00Z"}}"#,
        )
        .unwrap();
        let snap = build_snapshot(&u, Some(&sub));
        assert_eq!(snap.plan_type, "Pro Max");
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[1].name, "Weekly");
        assert_eq!(snap.tiers[1].quota, 50);
        assert_eq!(snap.tiers[1].remaining, 40); // 50 - 10
    }

    #[test]
    fn degenerate_total_falls_back_to_status_only() {
        let u = parse_usage(r#"{"remaining":42.5}"#).unwrap();
        let snap = build_snapshot(&u, None);
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.status_text.as_deref(), Some("42.5 credits remaining"));
    }

    #[test]
    fn epoch_millis_reset_parsed() {
        // 1_800_000_000_000 ms = 2027-01-15T08:00:00Z.
        let u =
            parse_usage(r#"{"quota":10,"remaining":5,"next_quota_reset":1800000000000}"#).unwrap();
        assert!(build_snapshot(&u, None)
            .session_reset
            .unwrap()
            .starts_with("2027-01-15T08:00:00"));
    }

    #[test]
    fn title_case_matches_capitalized_across_word_boundaries() {
        // Whitespace, hyphen, apostrophe, and underscore all start a new word
        // (Swift `.capitalized` semantics), so plan_type converges with the Mac.
        assert_eq!(title_case("pro max"), "Pro Max");
        assert_eq!(title_case("pro-max"), "Pro-Max");
        assert_eq!(title_case("o'reilly plan"), "O'Reilly Plan");
        assert_eq!(title_case("creator_pro"), "Creator_Pro");
        assert_eq!(title_case("TEAM"), "Team");
    }

    #[test]
    fn compact_credits_formatting() {
        assert_eq!(compact_credits(1500.0), "1,500");
        assert_eq!(compact_credits(12.5), "12.5");
        assert_eq!(compact_credits(999.0), "999");
        assert_eq!(compact_credits(12345.0), "12,345");
    }
}
