//! Crof quota collection — port of macOS `CrofCollector` (itself derived
//! from steipete/CodexBar, MIT).
//!
//! Endpoint: `GET https://crof.ai/usage_api/`
//! Auth: `Authorization: Bearer <apiKey>` — env `CROF_API_KEY` or the
//! Settings-stored `crof_api_key`.
//!
//! Response: `{ "credits": f64, "requests_plan": f64,
//! "usable_requests": f64 }`. Requests are the real depleting quota
//! (`quota = requests_plan`, `remaining = clamp(usable_requests, 0,
//! plan)`), resetting at the next **America/Chicago** local midnight
//! (DST-safe via `chrono-tz`, matching the Mac). The uncapped credit
//! balance has no cap to gauge against, so — because the desktop
//! `QuotaSnapshot` has no `status_text` field like the Mac's — it is
//! surfaced as a second `Credits` tier with `quota == remaining` (the
//! same convention the DeepSeek collector uses for pure balances).

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::America::Chicago;
use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const USAGE_URL: &str = "https://crof.ai/usage_api/";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct CrofUsage {
    #[serde(default)]
    credits: f64,
    #[serde(default, rename = "requests_plan")]
    requests_plan: f64,
    #[serde(default, rename = "usable_requests")]
    usable_requests: f64,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[Crof] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_usage(&token).await?;
    let parsed: CrofUsage =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    Ok(Some(map_to_snapshot(&parsed, Utc::now())))
}

fn resolve_key() -> Option<String> {
    if let Ok(k) = std::env::var("CROF_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.crof_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_usage(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(USAGE_URL)
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

/// Next America/Chicago local midnight strictly after `now`. DST-safe:
/// US DST transitions occur at 02:00 local, so a 00:00 local instant is
/// always unambiguous and always exists — but we still handle every
/// `LocalResult` arm defensively (falling back to `now + 24h`).
fn next_request_reset(now: DateTime<Utc>) -> DateTime<Utc> {
    let local_today = now.with_timezone(&Chicago).date_naive();
    let tomorrow = local_today.succ_opt().unwrap_or(local_today);
    let midnight = match tomorrow.and_hms_opt(0, 0, 0) {
        Some(m) => m,
        None => return now + chrono::Duration::days(1),
    };
    match Chicago.from_local_datetime(&midnight) {
        chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc),
        chrono::LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
        chrono::LocalResult::None => now + chrono::Duration::days(1),
    }
}

fn map_to_snapshot(u: &CrofUsage, now: DateTime<Utc>) -> QuotaSnapshot {
    let plan = u.requests_plan.max(0.0);
    let usable = u.usable_requests.max(0.0).min(plan);
    let quota_int = plan.round() as i64;
    let remaining_int = usable.round() as i64;
    let reset = next_request_reset(now).to_rfc3339();

    let mut tiers: Vec<TierEntry> = Vec::new();
    if quota_int > 0 {
        tiers.push(TierEntry {
            name: "Requests".to_string(),
            quota: quota_int,
            remaining: remaining_int,
            reset_time: Some(reset.clone()),
        });
    }
    // Uncapped credit balance → its own full tier (quota == remaining),
    // mirroring the DeepSeek pure-balance convention. Only when positive.
    let credits_int = u.credits.max(0.0).round() as i64;
    if credits_int > 0 {
        tiers.push(TierEntry {
            name: "Credits".to_string(),
            quota: credits_int,
            remaining: credits_int,
            reset_time: None,
        });
    }

    // Readable line surfacing the uncapped credit balance alongside the
    // requests gauge (mirrors the Mac's "$X credits · N requests left").
    let credits = u.credits.max(0.0);
    let status_text = if credits > 0.0 {
        Some(format!(
            "${credits:.2} credits · {remaining_int} requests left"
        ))
    } else {
        None
    };

    QuotaSnapshot {
        status_text,
        plan_type: "API key".to_string(),
        remaining: remaining_int,
        quota: quota_int,
        // Mirror Mac: `reset_time` is always set to the requests reset.
        session_reset: Some(reset),
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn parses_requests_and_surfaces_credits_tier() {
        let u: CrofUsage =
            serde_json::from_str(r#"{"credits":12.0,"requests_plan":1000,"usable_requests":750}"#)
                .unwrap();
        let snap = map_to_snapshot(&u, utc(2026, 7, 4, 18, 0, 0));
        assert_eq!(snap.plan_type, "API key");
        assert_eq!(snap.quota, 1000);
        assert_eq!(snap.remaining, 750);
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "Requests");
        assert_eq!(snap.tiers[0].quota, 1000);
        assert_eq!(snap.tiers[0].remaining, 750);
        // Credits surfaced as a full tier (quota == remaining).
        assert_eq!(snap.tiers[1].name, "Credits");
        assert_eq!(snap.tiers[1].quota, 12);
        assert_eq!(snap.tiers[1].remaining, 12);
        assert!(snap.tiers[1].reset_time.is_none());
        assert_eq!(
            snap.status_text.as_deref(),
            Some("$12.00 credits · 750 requests left")
        );
    }

    #[test]
    fn clamps_usable_to_plan_and_floors_negatives() {
        // usable > plan → clamped to plan; negative plan → 0.
        let u: CrofUsage =
            serde_json::from_str(r#"{"credits":0,"requests_plan":1000,"usable_requests":5000}"#)
                .unwrap();
        let snap = map_to_snapshot(&u, utc(2026, 7, 4, 18, 0, 0));
        assert_eq!(snap.quota, 1000);
        assert_eq!(snap.remaining, 1000); // min(5000, 1000)

        let u: CrofUsage =
            serde_json::from_str(r#"{"credits":0,"requests_plan":-5,"usable_requests":-9}"#)
                .unwrap();
        let snap = map_to_snapshot(&u, utc(2026, 7, 4, 18, 0, 0));
        assert_eq!(snap.quota, 0);
        assert_eq!(snap.remaining, 0);
    }

    #[test]
    fn no_plan_no_credits_yields_no_tiers() {
        let u: CrofUsage = serde_json::from_str(r#"{}"#).unwrap();
        let snap = map_to_snapshot(&u, utc(2026, 7, 4, 18, 0, 0));
        assert_eq!(snap.quota, 0);
        assert_eq!(snap.remaining, 0);
        assert!(snap.tiers.is_empty());
        // Credits-only account still gets exactly the Credits tier.
        let u: CrofUsage = serde_json::from_str(r#"{"credits":42.4}"#).unwrap();
        let snap = map_to_snapshot(&u, utc(2026, 7, 4, 18, 0, 0));
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Credits");
        assert_eq!(snap.tiers[0].quota, 42); // 42.4 rounds down
    }

    #[test]
    fn reset_is_next_chicago_midnight_cdt() {
        // 2026-07-04 18:00Z = 13:00 CDT (UTC-5) → next midnight is
        // 2026-07-05 00:00 CDT = 2026-07-05 05:00Z.
        let u: CrofUsage =
            serde_json::from_str(r#"{"requests_plan":10,"usable_requests":3}"#).unwrap();
        let snap = map_to_snapshot(&u, utc(2026, 7, 4, 18, 0, 0));
        assert_eq!(
            snap.session_reset.as_deref(),
            Some("2026-07-05T05:00:00+00:00")
        );
    }

    #[test]
    fn reset_is_next_chicago_midnight_cst() {
        // 2026-01-15 18:00Z = 12:00 CST (UTC-6) → next midnight is
        // 2026-01-16 00:00 CST = 2026-01-16 06:00Z. Confirms DST shift.
        let u: CrofUsage =
            serde_json::from_str(r#"{"requests_plan":10,"usable_requests":3}"#).unwrap();
        let snap = map_to_snapshot(&u, utc(2026, 1, 15, 18, 0, 0));
        assert_eq!(
            snap.session_reset.as_deref(),
            Some("2026-01-16T06:00:00+00:00")
        );
    }

    #[test]
    fn reset_just_before_local_midnight_rolls_to_next_day() {
        // 2026-07-05 04:30Z = 2026-07-04 23:30 CDT → next midnight is
        // still 2026-07-05 05:00Z (the very next local midnight).
        let u: CrofUsage =
            serde_json::from_str(r#"{"requests_plan":10,"usable_requests":3}"#).unwrap();
        let snap = map_to_snapshot(&u, utc(2026, 7, 5, 4, 30, 0));
        assert_eq!(
            snap.session_reset.as_deref(),
            Some("2026-07-05T05:00:00+00:00")
        );
    }
}
