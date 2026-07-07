//! T3 Chat usage collection — port of macOS `T3ChatCollector` (itself
//! derived from steipete/CodexBar, MIT).
//!
//! Endpoint: `GET t3.chat/api/trpc/getCustomerData?batch=1&input=…` (tRPC →
//! JSONL). Auth: **cookie-session** — `Cookie:` header (ALL t3.chat cookies,
//! incl. Vercel clearance) from env `T3CHAT_COOKIE` or the Settings-stored
//! `t3chat_cookie`. Manual paste only (no browser import).
//!
//! A real percent-window `.quota` collector (like Claude/Codex): two windows —
//! a 4-hour and a monthly — each reported as a *used* percentage, mapped to a
//! `remaining = 100 − used` gauge (`quota = 100`). The 4-hour window is the
//! headline. The response is JSONL with the customer object nested somewhere
//! inside the tRPC envelope, so a recursive finder locates it.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const BASE_URL: &str = "https://t3.chat/api/trpc/getCustomerData";
// Captured getCustomerData tRPC input shape (CodexBar, May 2026).
const INPUT_PARAM: &str =
    r#"{"0":{"json":{"sessionId":null},"meta":{"values":{"sessionId":["undefined"]}}}}"#;
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct Subscription {
    #[serde(default, rename = "productName")]
    product_name: Option<String>,
    #[serde(default, rename = "currentPeriodEnd")]
    current_period_end: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CustomerData {
    #[serde(default, rename = "subTier")]
    sub_tier: Option<String>,
    #[serde(default)]
    subscription: Option<Subscription>,
    #[serde(default, rename = "usageFourHourPercentage")]
    four_hour_pct: Option<f64>,
    #[serde(default, rename = "usageMonthPercentage")]
    month_pct: Option<f64>,
    #[serde(default, rename = "usagePeriodPercentage")]
    period_pct: Option<f64>,
    #[serde(default, rename = "usageFourHourNextResetAt")]
    four_hour_reset: Option<f64>,
    #[serde(default, rename = "usageWindowNextResetAt")]
    window_reset: Option<f64>,
}

impl CustomerData {
    /// `productName` (or `subTier`), title-cased, hyphens → spaces.
    fn plan_name(&self) -> Option<String> {
        let raw = self
            .subscription
            .as_ref()
            .and_then(|s| s.product_name.clone())
            .or_else(|| self.sub_tier.clone())?;
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        Some(
            raw.split('-')
                .map(|w| {
                    let mut ch = w.chars();
                    match ch.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let cookie = match resolve_cookie() {
        Some(c) => c,
        None => {
            log::debug!("[T3 Chat] no session cookie (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch(&cookie).await?;
    let customer = parse_jsonl(&body)?;
    Ok(Some(map_to_snapshot(&customer)))
}

fn resolve_cookie() -> Option<String> {
    if let Ok(c) = std::env::var("T3CHAT_COOKIE") {
        let c = c.trim().to_string();
        if !c.is_empty() {
            return Some(c);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.t3chat_cookie)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch(cookie: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(BASE_URL)
        .query(&[("batch", "1"), ("input", INPUT_PARAM)])
        .header("Accept", "*/*")
        .header("trpc-accept", "application/jsonl")
        .header("x-trpc-source", "web-client")
        .header("x-trpc-batch", "true")
        .header("Referer", "https://t3.chat/settings/customization")
        .header("Origin", "https://t3.chat")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
        )
        .header("Cookie", cookie)
        .send()
        .await
        .map_err(|e| CollectorError::Http(format!("request: {e}")))?;
    let status = resp.status();
    if status.as_u16() == 429
        && resp
            .headers()
            .get("x-vercel-mitigated")
            .and_then(|v| v.to_str().ok())
            == Some("challenge")
    {
        return Err(CollectorError::SchemaOrIo(
            "T3 Chat: blocked by Vercel bot protection — open t3.chat in your browser, \
             confirm you're logged in, then refresh your session cookie."
                .to_string(),
        ));
    }
    if !status.is_success() {
        return Err(CollectorError::Http(format!("HTTP {}", status.as_u16())));
    }
    resp.text()
        .await
        .map_err(|e| CollectorError::Http(format!("body: {e}")))
}

/// Recursively locate the customer object (identified by the usage-percentage
/// keys, or a subscription+usageBand pair) anywhere in a parsed JSONL line.
fn find_customer_data(v: &Value) -> Option<&Value> {
    match v {
        Value::Object(map) => {
            if map.contains_key("usageFourHourPercentage")
                || map.contains_key("usageMonthPercentage")
                || (map.contains_key("subscription") && map.contains_key("usageBand"))
            {
                return Some(v);
            }
            map.values().find_map(find_customer_data)
        }
        Value::Array(arr) => arr.iter().find_map(find_customer_data),
        _ => None,
    }
}

fn parse_jsonl(text: &str) -> Result<CustomerData, CollectorError> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(obj) = find_customer_data(&value) {
            return serde_json::from_value::<CustomerData>(obj.clone())
                .map_err(|e| CollectorError::SchemaOrIo(format!("T3 Chat: {e}")));
        }
    }
    Err(CollectorError::SchemaOrIo(
        "T3 Chat: missing customer data object".to_string(),
    ))
}

/// A *used* percentage → integer *remaining* percent (`100 − used`, clamped).
fn remaining_pct(used: Option<f64>) -> i64 {
    let u = used.unwrap_or(0.0).clamp(0.0, 100.0);
    (100.0 - u).round() as i64
}

/// JS epoch milliseconds (> 1e10) → seconds → RFC 3339. `None` for ≤ 0.
fn date_from_ms(raw: Option<f64>) -> Option<String> {
    let raw = raw?;
    if raw <= 0.0 {
        return None;
    }
    let secs = if raw > 10_000_000_000.0 {
        raw / 1000.0
    } else {
        raw
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0).map(|d| d.to_rfc3339())
}

fn map_to_snapshot(c: &CustomerData) -> QuotaSnapshot {
    let r4h = remaining_pct(c.four_hour_pct);
    let r_month = remaining_pct(c.month_pct.or(c.period_pct));
    let reset4h = date_from_ms(c.four_hour_reset.or(c.window_reset));
    let reset_month = date_from_ms(c.subscription.as_ref().and_then(|s| s.current_period_end));

    let tiers = vec![
        TierEntry {
            name: "4-hour".to_string(),
            quota: 100,
            remaining: r4h,
            reset_time: reset4h.clone(),
        },
        TierEntry {
            name: "Monthly".to_string(),
            quota: 100,
            remaining: r_month,
            reset_time: reset_month,
        },
    ];

    QuotaSnapshot {
        status_text: None,
        // Headline = the 4-hour window.
        plan_type: c.plan_name().unwrap_or_else(|| "T3 Chat".to_string()),
        remaining: r4h,
        quota: 100,
        session_reset: reset4h,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A realistic tRPC/JSONL line with the customer object nested inside.
    const LINE: &str = r#"{"result":{"data":{"json":{"customer":{"subTier":"pro","subscription":{"productName":"pro-plan","currentPeriodEnd":1800000000000},"usageBand":"normal","usageFourHourPercentage":30,"usageMonthPercentage":90,"usageFourHourNextResetAt":1800000000000}}}}}"#;

    #[test]
    fn parses_jsonl_and_maps_percent_windows() {
        let c = parse_jsonl(&format!("garbage line\n{LINE}\n")).unwrap();
        let snap = map_to_snapshot(&c);
        // used 30% (4h) → 70 remaining; used 90% (month) → 10 remaining.
        assert_eq!(snap.quota, 100);
        assert_eq!(snap.remaining, 70); // headline = 4-hour
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "4-hour");
        assert_eq!(snap.tiers[0].remaining, 70);
        assert_eq!(snap.tiers[1].name, "Monthly");
        assert_eq!(snap.tiers[1].remaining, 10);
        assert_eq!(snap.plan_type, "Pro Plan"); // title-cased, hyphen→space
        assert!(snap.session_reset.is_some());
        assert!(snap.tiers[1].reset_time.is_some()); // currentPeriodEnd
    }

    #[test]
    fn remaining_clamps_used_percentage() {
        assert_eq!(remaining_pct(Some(30.0)), 70);
        assert_eq!(remaining_pct(Some(150.0)), 0); // over 100 → 0 left
        assert_eq!(remaining_pct(Some(-10.0)), 100); // negative → full
        assert_eq!(remaining_pct(None), 100); // absent → full
    }

    #[test]
    fn month_falls_back_to_period_percentage() {
        let c = CustomerData {
            month_pct: None,
            period_pct: Some(40.0),
            ..Default::default()
        };
        assert_eq!(map_to_snapshot(&c).tiers[1].remaining, 60);
    }

    #[test]
    fn plan_name_prefers_product_then_subtier_then_default() {
        let c = CustomerData {
            sub_tier: Some("free".to_string()),
            ..Default::default()
        };
        assert_eq!(c.plan_name().as_deref(), Some("Free"));
        let c = CustomerData::default();
        assert_eq!(map_to_snapshot(&c).plan_type, "T3 Chat"); // fallback
    }

    #[test]
    fn ms_and_second_epochs_both_handled() {
        // milliseconds (> 1e10) → divided; seconds → used as-is.
        assert!(date_from_ms(Some(1_800_000_000_000.0)).is_some());
        assert!(date_from_ms(Some(1_800_000_000.0)).is_some());
        assert_eq!(date_from_ms(Some(0.0)), None);
        assert_eq!(date_from_ms(None), None);
    }

    #[test]
    fn missing_customer_data_is_error() {
        assert!(parse_jsonl(r#"{"result":{"data":{"json":{"unrelated":1}}}}"#).is_err());
        assert!(parse_jsonl("not json at all").is_err());
    }
}
