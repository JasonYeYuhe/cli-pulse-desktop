//! Kimi K2 credit collection — port of macOS `KimiK2Collector`.
//!
//! Endpoint: `GET https://kimi-k2.ai/api/user/credits`
//! Auth: `Authorization: Bearer <apiKey>` — env `KIMI_K2_API_KEY` /
//! `KIMI_API_KEY` / `KIMI_KEY`, or the Settings-stored `kimi_k2_api_key`.
//!
//! Depleting credits: the API returns a consumed + remaining split (total =
//! consumed + remaining is the cap), so this is a REAL quota gauge (unlike
//! DeepSeek's pure balance). The response shape varies — fields may be flat,
//! nested under `data`, or under `data.usage`, and use any of several candidate
//! key names — so a flexible search extracts `consumed`/`remaining` (vendored
//! from upstream).
//!
//! Scale: **units (× 100_000)** — matches the Mac Kimi K2 collector (verified
//! `KimiK2Collector.swift` `scale = 100_000`), so a dual-writer converges.
//! (Each desktop collector mirrors its own Mac twin's scale — DeepSeek/Venice
//! cents, Moonshot/Kimi-K2 units.)

use std::time::Duration;

use serde_json::Value;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const CREDITS_URL: &str = "https://kimi-k2.ai/api/user/credits";
const SCALE: f64 = 100_000.0;
const TIMEOUT: Duration = Duration::from_secs(15);

const CONSUMED_KEYS: &[&str] = &[
    "total_credits_consumed",
    "totalCreditsConsumed",
    "total_credits_used",
    "credits_consumed",
    "creditsConsumed",
    "consumedCredits",
    "usedCredits",
    "consumed",
    "total",
    "used",
];
const REMAINING_KEYS: &[&str] = &[
    "credits_remaining",
    "creditsRemaining",
    "remaining_credits",
    "available_credits",
    "availableCredits",
    "credits_left",
    "remaining",
    "available",
    "balance",
];

#[derive(Debug, Clone, PartialEq)]
struct Credits {
    consumed: f64,
    remaining: f64,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[Kimi K2] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_credits(&token).await?;
    let credits = parse_credits(&body)?;
    Ok(Some(map_to_snapshot(&credits)))
}

fn resolve_key() -> Option<String> {
    for env in ["KIMI_K2_API_KEY", "KIMI_API_KEY", "KIMI_KEY"] {
        if let Ok(k) = std::env::var(env) {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.kimi_k2_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_credits(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(CREDITS_URL)
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

/// Flexible field search: prefer `data.usage`, then `data`, then the flat
/// root — mirroring the Mac's nesting priority. A present-but-fieldless root
/// is an error (matches upstream throwing).
fn parse_credits(body: &str) -> Result<Credits, CollectorError> {
    let json: Value =
        serde_json::from_str(body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    let root: &Value = match json.get("data").filter(|d| d.is_object()) {
        Some(inner) => match inner.get("usage").filter(|u| u.is_object()) {
            Some(usage) => usage,
            None => inner,
        },
        None => &json,
    };
    extract_credits(root)
        .ok_or_else(|| CollectorError::SchemaOrIo("Kimi K2: no consumed/remaining fields".into()))
}

fn first_num(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_f64))
}

fn extract_credits(v: &Value) -> Option<Credits> {
    let obj = v.as_object()?;
    let consumed = first_num(obj, CONSUMED_KEYS);
    let remaining = first_num(obj, REMAINING_KEYS);
    if consumed.is_none() && remaining.is_none() {
        return None;
    }
    Some(Credits {
        consumed: consumed.unwrap_or(0.0),
        remaining: remaining.unwrap_or(0.0),
    })
}

/// Value → integer units (`round(x * 100_000)`), floored at 0, saturating at
/// `i64::MAX`. Non-finite → 0.
fn units(x: f64) -> i64 {
    if !x.is_finite() || x <= 0.0 {
        return 0;
    }
    let u = (x * SCALE).round();
    if u >= i64::MAX as f64 {
        i64::MAX
    } else {
        u as i64
    }
}

fn map_to_snapshot(c: &Credits) -> QuotaSnapshot {
    let total = (c.consumed + c.remaining).max(0.0);
    let total_units = units(total);
    let remaining_units = units(c.remaining);

    let mut tiers: Vec<TierEntry> = Vec::new();
    if total_units > 0 {
        tiers.push(TierEntry {
            name: "Credits".to_string(),
            quota: total_units,
            remaining: remaining_units,
            reset_time: None,
        });
    }

    // Readable line (the gauge shows raw ×100_000 units) — mirrors the Mac's
    // "X / Y credits" using the pre-scale dollar figures.
    let status_text = if total > 0.0 {
        Some(format!(
            "{:.2} / {:.2} credits",
            c.remaining.max(0.0),
            total
        ))
    } else {
        None
    };

    QuotaSnapshot {
        status_text,
        plan_type: "Credits".to_string(),
        remaining: remaining_units,
        quota: total_units,
        session_reset: None,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_consumed_remaining_maps_to_credits_tier() {
        let c = parse_credits(r#"{"consumed":3.0,"remaining":5.0}"#).unwrap();
        let snap = map_to_snapshot(&c);
        assert_eq!(snap.plan_type, "Credits");
        // total 8.0 → 800_000 units; remaining 5.0 → 500_000.
        assert_eq!(snap.quota, 800_000);
        assert_eq!(snap.remaining, 500_000);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Credits");
        assert_eq!(snap.tiers[0].quota, 800_000);
        assert_eq!(snap.tiers[0].remaining, 500_000);
        assert_eq!(snap.status_text.as_deref(), Some("5.00 / 8.00 credits"));
    }

    #[test]
    fn nested_data_usage_and_alias_keys() {
        // Nested under data.usage, with alias key names.
        let c = parse_credits(
            r#"{"data":{"usage":{"total_credits_consumed":2.0,"credits_remaining":18.0}}}"#,
        )
        .unwrap();
        assert_eq!(c.consumed, 2.0);
        assert_eq!(c.remaining, 18.0);
        // Nested under data (no usage), balance alias for remaining.
        let c = parse_credits(r#"{"data":{"used":1.0,"balance":9.0}}"#).unwrap();
        assert_eq!(c.consumed, 1.0);
        assert_eq!(c.remaining, 9.0);
    }

    #[test]
    fn remaining_only_is_full_bar() {
        let c = parse_credits(r#"{"remaining":4.0}"#).unwrap();
        let snap = map_to_snapshot(&c);
        // consumed defaults 0 → total == remaining → full bar.
        assert_eq!(snap.quota, 400_000);
        assert_eq!(snap.remaining, 400_000);
    }

    #[test]
    fn no_fields_is_error() {
        assert!(parse_credits(r#"{"unrelated":1}"#).is_err());
        // data present but usage fieldless → error (does NOT fall back to root).
        assert!(parse_credits(r#"{"data":{"usage":{"nope":1}}}"#).is_err());
    }

    #[test]
    fn non_number_fields_ignored() {
        // String values are not numbers → treated as absent (Mac uses NSNumber).
        assert!(parse_credits(r#"{"remaining":"5","consumed":"3"}"#).is_err());
    }

    #[test]
    fn units_floor_and_nonfinite() {
        assert_eq!(units(1.0), 100_000);
        assert_eq!(units(0.0), 0);
        assert_eq!(units(-2.0), 0);
        assert_eq!(units(f64::NAN), 0);
    }
}
