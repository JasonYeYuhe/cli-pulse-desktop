//! ElevenLabs character-quota collection — port of macOS `ElevenLabsCollector`
//! (itself derived from steipete/CodexBar, MIT).
//!
//! Endpoint: `GET https://api.elevenlabs.io/v1/user/subscription`
//! Auth: the custom **`xi-api-key`** header (NOT `Authorization: Bearer`) —
//! env `ELEVENLABS_API_KEY` / `XI_API_KEY`, or the Settings-stored
//! `elevenlabs_api_key`.
//!
//! A real depleting `.quota`: a monthly **character** cap
//! (`character_limit` / `character_count`) with a renewal
//! (`next_character_count_reset_unix`), plus optional voice-slot sub-tiers. The
//! status line shows "X / Y characters" (+ an overage suffix when present).

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const SUBSCRIPTION_URL: &str = "https://api.elevenlabs.io/v1/user/subscription";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct Overage {
    #[serde(default)]
    amount: Option<String>,
    #[serde(default)]
    currency: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SubscriptionResponse {
    #[serde(default)]
    tier: Option<String>,
    #[serde(default, rename = "character_count")]
    character_count: i64,
    #[serde(default, rename = "character_limit")]
    character_limit: i64,
    #[serde(default, rename = "voice_slots_used")]
    voice_slots_used: Option<i64>,
    #[serde(default, rename = "professional_voice_slots_used")]
    professional_voice_slots_used: Option<i64>,
    #[serde(default, rename = "voice_limit")]
    voice_limit: Option<i64>,
    #[serde(default, rename = "professional_voice_limit")]
    professional_voice_limit: Option<i64>,
    #[serde(default, rename = "current_overage")]
    current_overage: Option<Overage>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default, rename = "next_character_count_reset_unix")]
    next_reset_unix: Option<i64>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[ElevenLabs] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_subscription(&token).await?;
    let resp: SubscriptionResponse =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    Ok(Some(map_to_snapshot(&resp)))
}

fn resolve_key() -> Option<String> {
    for env in ["ELEVENLABS_API_KEY", "XI_API_KEY"] {
        if let Ok(k) = std::env::var(env) {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.elevenlabs_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_subscription(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(SUBSCRIPTION_URL)
        // The quirk: ElevenLabs uses `xi-api-key`, not `Authorization: Bearer`.
        .header("xi-api-key", token)
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

/// Integer with thousands separators (e.g. `1234567` → `"1,234,567"`).
fn format_count(n: i64) -> String {
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

/// Title-case each whitespace-separated word (first char upper, rest lower).
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &ch.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// snake_case tier → Title Case, plus " · {status}" when status is present and
/// not "active". Falls back to the raw status when tier is empty; `None` when
/// both are absent.
fn display_tier(tier: Option<&str>, status: Option<&str>) -> Option<String> {
    let trimmed = tier.unwrap_or("").trim();
    if trimmed.is_empty() {
        return status
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    let titled = title_case(&trimmed.replace('_', " "));
    if let Some(s) = status {
        let s = s.trim();
        if !s.is_empty() && s.to_lowercase() != "active" {
            return Some(format!("{titled} · {s}"));
        }
    }
    Some(titled)
}

fn ts_iso(secs: i64) -> Option<String> {
    if secs <= 0 {
        return None;
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0).map(|d| d.to_rfc3339())
}

fn map_to_snapshot(r: &SubscriptionResponse) -> QuotaSnapshot {
    let limit = r.character_limit.max(0);
    let remaining = (r.character_limit - r.character_count).max(0);
    let reset = r.next_reset_unix.and_then(ts_iso);

    let mut tiers: Vec<TierEntry> = Vec::new();
    if limit > 0 {
        tiers.push(TierEntry {
            name: "Characters".to_string(),
            quota: limit,
            remaining,
            reset_time: reset.clone(),
        });
    }
    if let (Some(used), Some(vl)) = (r.voice_slots_used, r.voice_limit) {
        if vl > 0 {
            tiers.push(TierEntry {
                name: "Voice slots".to_string(),
                quota: vl,
                remaining: (vl - used).max(0),
                reset_time: None,
            });
        }
    }
    if let (Some(used), Some(pl)) = (r.professional_voice_slots_used, r.professional_voice_limit) {
        if pl > 0 {
            tiers.push(TierEntry {
                name: "Professional voices".to_string(),
                quota: pl,
                remaining: (pl - used).max(0),
                reset_time: None,
            });
        }
    }

    let mut status = format!(
        "{} / {} characters",
        format_count(r.character_count),
        format_count(r.character_limit)
    );
    if let Some(o) = &r.current_overage {
        let amount = o.amount.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let currency = o
            .currency
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let (Some(a), Some(c)) = (amount, currency) {
            status.push_str(&format!(" (Overage: {a} {c})"));
        }
    }

    QuotaSnapshot {
        status_text: Some(status),
        plan_type: display_tier(r.tier.as_deref(), r.status.as_deref())
            .unwrap_or_else(|| "API key".to_string()),
        remaining,
        quota: limit,
        session_reset: reset,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_character_quota_and_voice_tiers() {
        let r: SubscriptionResponse = serde_json::from_str(
            r#"{
                "tier":"creator",
                "character_count":30000,
                "character_limit":100000,
                "voice_slots_used":3,
                "voice_limit":10,
                "next_character_count_reset_unix":1800000000
            }"#,
        )
        .unwrap();
        let snap = map_to_snapshot(&r);
        assert_eq!(snap.quota, 100_000);
        assert_eq!(snap.remaining, 70_000); // 100000 - 30000
        assert_eq!(snap.plan_type, "Creator");
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "Characters");
        assert_eq!(snap.tiers[0].remaining, 70_000);
        assert_eq!(snap.tiers[1].name, "Voice slots");
        assert_eq!(snap.tiers[1].remaining, 7); // 10 - 3
        assert_eq!(
            snap.status_text.as_deref(),
            Some("30,000 / 100,000 characters")
        );
        assert!(snap.session_reset.is_some());
    }

    #[test]
    fn overage_suffix_appended_when_present() {
        let r: SubscriptionResponse = serde_json::from_str(
            r#"{"character_count":5,"character_limit":10,"current_overage":{"amount":"1.50","currency":"USD"}}"#,
        )
        .unwrap();
        assert_eq!(
            map_to_snapshot(&r).status_text.as_deref(),
            Some("5 / 10 characters (Overage: 1.50 USD)")
        );
    }

    #[test]
    fn display_tier_variants() {
        assert_eq!(
            display_tier(Some("creator_pro"), None).as_deref(),
            Some("Creator Pro")
        );
        // non-active status appended.
        assert_eq!(
            display_tier(Some("starter"), Some("past_due")).as_deref(),
            Some("Starter · past_due")
        );
        // "active" status is suppressed.
        assert_eq!(
            display_tier(Some("pro"), Some("active")).as_deref(),
            Some("Pro")
        );
        // empty tier falls back to status; both empty → None.
        assert_eq!(
            display_tier(Some(""), Some("trialing")).as_deref(),
            Some("trialing")
        );
        assert_eq!(display_tier(None, None), None);
    }

    #[test]
    fn no_limit_falls_back_to_api_key_plan_and_zero_quota() {
        let r: SubscriptionResponse =
            serde_json::from_str(r#"{"character_count":0,"character_limit":0}"#).unwrap();
        let snap = map_to_snapshot(&r);
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.plan_type, "API key");
    }

    #[test]
    fn format_count_groups_thousands() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(1500), "1,500");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }
}
