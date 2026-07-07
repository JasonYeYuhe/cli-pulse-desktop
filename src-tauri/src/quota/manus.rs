//! Manus credit pools — port of macOS `ManusCollector` (derived from
//! steipete/CodexBar, MIT).
//!
//! Endpoint: `POST https://api.manus.im/user.v1.UserService/GetAvailableCredits`
//! (Connect-RPC, JSON body `{}`). Auth novelty: the **`session_id` cookie value**
//! is sent as `Authorization: Bearer <value>` (not a `Cookie:` header). The
//! cookie comes from env `MANUS_SESSION_TOKEN` / `MANUS_SESSION_ID` /
//! `MANUS_COOKIE` or the Settings `manus_cookie` (manual paste — the desktop has
//! no browser auto-import).
//!
//! Manus returns two capped credit pools (monthly pro + periodic refresh) → a
//! real `.quota`: the monthly pool (else the refresh pool) drives the headline
//! gauge, the refresh pool is a secondary tier. With neither pool capped it
//! degrades to a status-only balance. `status_text` is always
//! "Balance: N credits" (+ a refresh detail when present).
//!
//! Accepted divergence: `nextRefreshTime` is parsed with `parse_from_rfc3339`,
//! which accepts fractional seconds that the Mac's default `ISO8601DateFormatter`
//! rejects (→ nil). We keep the more-permissive parse rather than drop a valid
//! reset time to mirror that limitation; the two only differ if Manus emits
//! sub-second timestamps, which it is not observed to do.

use std::time::Duration;

use serde_json::Value;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const CREDITS_URL: &str = "https://api.manus.im/user.v1.UserService/GetAvailableCredits";
const TIMEOUT: Duration = Duration::from_secs(15);
const ENV_VARS: [&str; 3] = ["MANUS_SESSION_TOKEN", "MANUS_SESSION_ID", "MANUS_COOKIE"];
const EXPECTED_KEYS: [&str; 8] = [
    "totalCredits",
    "freeCredits",
    "periodicCredits",
    "addonCredits",
    "refreshCredits",
    "maxRefreshCredits",
    "proMonthlyCredits",
    "eventCredits",
];

#[derive(Debug, Clone, Default)]
struct ManusCredits {
    total: f64,
    periodic: f64,
    refresh: f64,
    max_refresh: f64,
    pro_monthly: f64,
    next_refresh: Option<String>,
    refresh_interval: Option<String>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let raw = match resolve_cookie() {
        Some(c) => c,
        None => {
            log::debug!("[Manus] no session cookie (env or Settings) — skipping");
            return Ok(None);
        }
    };
    let token = match session_token(&raw) {
        Some(t) => t,
        None => {
            log::debug!("[Manus] cookie has no session_id value — skipping");
            return Ok(None);
        }
    };
    let body = fetch_credits(&token).await?;
    let credits = parse_response(&body)?;
    Ok(Some(build_snapshot(&credits)))
}

fn resolve_cookie() -> Option<String> {
    for env in ENV_VARS {
        if let Ok(v) = std::env::var(env) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.manus_cookie)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Extract the `session_id` value (used as the Bearer token) from a resolved
/// Cookie header, or accept a bare token directly. Ported from the Mac's
/// `ManusCookieHeader.token(from:)`.
fn session_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Clearly a bare token: no cookie syntax at all.
    if !trimmed.contains('=') && !trimmed.contains(';') {
        return Some(trimmed.to_string());
    }
    // Parse "name=value; name2=value2" pairs; split each on the FIRST '=' so a
    // base64 value containing '=' survives intact.
    for part in trimmed.split(';') {
        let mut it = part.splitn(2, '=');
        let name = it.next().unwrap_or("").trim();
        if let Some(value) = it.next() {
            if name.eq_ignore_ascii_case("session_id") {
                let v = value.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    // Fallback: a single value (no ';') whose only '=' are trailing base64
    // padding ⇒ treat the whole thing as a bare token. A stray non-session
    // pair like `analytics_id=123` keeps an interior '=' after stripping
    // padding ⇒ rejected.
    if !trimmed.contains(';') {
        let stripped = trimmed.trim_matches('=');
        if !stripped.contains('=') {
            return Some(trimmed.to_string());
        }
    }
    None
}

async fn fetch_credits(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .post(CREDITS_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("Origin", "https://manus.im")
        .header("Referer", "https://manus.im/")
        .header("Connect-Protocol-Version", "1")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/135.0.0.0 Safari/537.36",
        )
        .body("{}")
        .send()
        .await
        .map_err(|e| CollectorError::Http(format!("request: {e}")))?;
    if !resp.status().is_success() {
        return Err(CollectorError::Http(format!(
            "HTTP {}",
            resp.status().as_u16()
        )));
    }
    resp.text()
        .await
        .map_err(|e| CollectorError::Http(format!("body: {e}")))
}

fn lossy_double(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()).unwrap_or(0.0),
        Some(Value::String(s)) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .unwrap_or(0.0),
        _ => 0.0,
    }
}

fn flexible_date(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                return None;
            }
            chrono::DateTime::parse_from_rfc3339(t)
                .ok()
                .map(|d| d.with_timezone(&chrono::Utc).to_rfc3339())
        }
        Some(Value::Number(n)) => n.as_f64().and_then(|secs| {
            chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0).map(|d| d.to_rfc3339())
        }),
        _ => None,
    }
}

fn extract_credits(obj: &Value) -> ManusCredits {
    ManusCredits {
        total: lossy_double(obj.get("totalCredits")),
        periodic: lossy_double(obj.get("periodicCredits")),
        refresh: lossy_double(obj.get("refreshCredits")),
        max_refresh: lossy_double(obj.get("maxRefreshCredits")),
        pro_monthly: lossy_double(obj.get("proMonthlyCredits")),
        next_refresh: flexible_date(obj.get("nextRefreshTime")),
        refresh_interval: obj
            .get("refreshInterval")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

fn parse_response(body: &str) -> Result<ManusCredits, CollectorError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| CollectorError::SchemaOrIo(format!("parse: {e}")))?;
    // Envelope first: data | result | response | availableCredits.
    for key in ["data", "result", "response", "availableCredits"] {
        if let Some(sub) = v.get(key) {
            if sub.is_object() {
                return Ok(extract_credits(sub));
            }
        }
    }
    // Bare payload: require ≥1 known credits key so an unrelated/error payload
    // doesn't decode to a bogus all-zero snapshot.
    let obj = v
        .as_object()
        .ok_or_else(|| CollectorError::SchemaOrIo("response not an object".into()))?;
    if !EXPECTED_KEYS.iter().any(|k| obj.contains_key(*k)) {
        return Err(CollectorError::SchemaOrIo(
            "response missing expected credits fields".into(),
        ));
    }
    Ok(extract_credits(&v))
}

fn clamped_int(value: f64, cap: i64) -> i64 {
    let rounded = if value.is_finite() {
        value.round() as i64
    } else {
        0
    };
    rounded.clamp(0, cap)
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

fn credit_count(value: f64) -> String {
    let rounded = if value.is_finite() {
        value.round()
    } else {
        0.0
    };
    group_int(rounded as i64)
}

/// Mirror Swift `String.capitalized`: uppercase the first alphanumeric of each
/// word (a word starts after any non-alphanumeric char — space, hyphen, etc.)
/// and lowercase the rest, so `"bi-weekly"` → `"Bi-Weekly"` like the Mac.
fn capitalize_words(s: &str) -> String {
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

fn build_snapshot(r: &ManusCredits) -> QuotaSnapshot {
    let balance = format!("Balance: {} credits", credit_count(r.total));
    let plan_type = if r.pro_monthly > 0.0 { "Pro" } else { "Free" }.to_string();

    let refresh_tier = |max_cap: i64| TierEntry {
        name: "Refresh".to_string(),
        quota: max_cap,
        remaining: clamped_int(r.refresh, max_cap),
        reset_time: r.next_refresh.clone(),
    };
    let refresh_detail = if r.max_refresh > 0.0 {
        let label = r
            .refresh_interval
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(capitalize_words)
            .unwrap_or_else(|| "Refresh".to_string());
        Some(format!(
            "{label}: {}/{}",
            credit_count(r.refresh),
            credit_count(r.max_refresh)
        ))
    } else {
        None
    };
    let status_text = |extra: &Option<String>| match extra {
        Some(e) => format!("{balance} · {e}"),
        None => balance.clone(),
    };

    // 1) Monthly pro pool is the primary gauge.
    if r.pro_monthly > 0.0 {
        let quota = clamped_int(r.pro_monthly, i64::MAX);
        let remaining = clamped_int(r.periodic, quota);
        let mut tiers = vec![TierEntry {
            name: "Monthly".to_string(),
            quota,
            remaining,
            reset_time: None,
        }];
        if r.max_refresh > 0.0 {
            tiers.push(refresh_tier(clamped_int(r.max_refresh, i64::MAX)));
        }
        return QuotaSnapshot {
            status_text: Some(status_text(&refresh_detail)),
            plan_type,
            remaining,
            quota,
            session_reset: None,
            tiers,
        };
    }

    // 2) No monthly pool, but a refresh pool ⇒ refresh-only quota.
    if r.max_refresh > 0.0 {
        let quota = clamped_int(r.max_refresh, i64::MAX);
        let remaining = clamped_int(r.refresh, quota);
        return QuotaSnapshot {
            status_text: Some(status_text(&refresh_detail)),
            plan_type,
            remaining,
            quota,
            session_reset: r.next_refresh.clone(),
            tiers: vec![refresh_tier(quota)],
        };
    }

    // 3) Neither pool capped ⇒ status-only balance.
    QuotaSnapshot {
        status_text: Some(balance),
        plan_type,
        remaining: 0,
        quota: 0,
        session_reset: None,
        tiers: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monthly_pool_is_primary_with_refresh_secondary() {
        let body = r#"{
            "totalCredits":1200,"proMonthlyCredits":1000,"periodicCredits":600,
            "maxRefreshCredits":300,"refreshCredits":120,"refreshInterval":"daily",
            "nextRefreshTime":"2026-07-08T00:00:00Z"
        }"#;
        let snap = build_snapshot(&parse_response(body).unwrap());
        assert_eq!(snap.plan_type, "Pro");
        assert_eq!(snap.quota, 1000);
        assert_eq!(snap.remaining, 600);
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "Monthly");
        assert_eq!(snap.tiers[1].name, "Refresh");
        assert_eq!(snap.tiers[1].quota, 300);
        assert_eq!(snap.tiers[1].remaining, 120);
        assert_eq!(
            snap.status_text.as_deref(),
            Some("Balance: 1,200 credits · Daily: 120/300")
        );
    }

    #[test]
    fn refresh_only_when_no_monthly_pool() {
        let body = r#"{"totalCredits":300,"maxRefreshCredits":300,"refreshCredits":250,"nextRefreshTime":"2026-07-08T00:00:00Z"}"#;
        let snap = build_snapshot(&parse_response(body).unwrap());
        assert_eq!(snap.plan_type, "Free");
        assert_eq!(snap.quota, 300);
        assert_eq!(snap.remaining, 250);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Refresh");
        assert_eq!(
            snap.session_reset.as_deref(),
            Some("2026-07-08T00:00:00+00:00")
        );
    }

    #[test]
    fn status_only_when_no_capped_pool() {
        let snap =
            build_snapshot(&parse_response(r#"{"totalCredits":42,"freeCredits":42}"#).unwrap());
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.status_text.as_deref(), Some("Balance: 42 credits"));
    }

    #[test]
    fn envelope_wrapped_payload_is_unwrapped() {
        let body = r#"{"data":{"totalCredits":10,"proMonthlyCredits":10,"periodicCredits":4}}"#;
        let snap = build_snapshot(&parse_response(body).unwrap());
        assert_eq!(snap.quota, 10);
        assert_eq!(snap.remaining, 4);
    }

    #[test]
    fn unrelated_payload_rejected() {
        assert!(matches!(
            parse_response(r#"{"error":"unauthorized","code":401}"#),
            Err(CollectorError::SchemaOrIo(_))
        ));
    }

    #[test]
    fn lossy_string_numbers_parsed() {
        let c = parse_response(
            r#"{"totalCredits":"500","proMonthlyCredits":"500","periodicCredits":"250"}"#,
        )
        .unwrap();
        assert_eq!(c.total, 500.0);
        assert_eq!(c.pro_monthly, 500.0);
    }

    #[test]
    fn multi_word_refresh_interval_title_cased() {
        // Swift `.capitalized` title-cases every word incl. across hyphens.
        let body = r#"{"totalCredits":300,"maxRefreshCredits":300,"refreshCredits":120,"refreshInterval":"bi-weekly"}"#;
        let snap = build_snapshot(&parse_response(body).unwrap());
        assert_eq!(
            snap.status_text.as_deref(),
            Some("Balance: 300 credits · Bi-Weekly: 120/300")
        );
    }

    #[test]
    fn session_token_extraction() {
        // session_id from a cookie header, first-'=' split preserves base64.
        assert_eq!(
            session_token("session_id=abc123==; other=x").as_deref(),
            Some("abc123==")
        );
        // bare token, no cookie syntax.
        assert_eq!(session_token("rawtoken123").as_deref(), Some("rawtoken123"));
        // single value that is only base64 padding → bare token.
        assert_eq!(session_token("dG9rZW4=").as_deref(), Some("dG9rZW4="));
        // stray non-session pair with interior '=' → rejected.
        assert_eq!(session_token("analytics_id=123"), None);
        assert_eq!(session_token("   "), None);
    }
}
