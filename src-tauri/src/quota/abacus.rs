//! Abacus AI compute-points — port of macOS `AbacusCollector` (derived from
//! steipete/CodexBar, MIT).
//!
//! Compute points (required): `GET apps.abacus.ai/api/_getOrganizationComputePoints`
//! → `totalComputePoints` / `computePointsLeft`. Billing (best-effort, ~5s grace):
//! `POST .../api/_getBillingInfo` → `nextBillingDate` / `currentTier`.
//! Auth: the whole resolved **`Cookie:` header** is sent as-is (no token
//! extraction, unlike Manus) — from env `ABACUS_COOKIE` / `ABACUS_SESSION_TOKEN`
//! or the Settings `abacus_cookie` (manual paste — no browser auto-import).
//!
//! Both calls run concurrently; the billing POST is bounded to a 5s per-request
//! timeout so it can never stall the required compute render. Both endpoints use
//! a `{success, result}` envelope. A positive compute-point cap maps to a real
//! `.quota` ("Compute Points" gauge); a degenerate `total <= 0` degrades to a
//! status-only balance.
//!
//! Accepted low-severity divergences vs the Mac (both cosmetic / edge-only):
//! (1) cookie resolution checks env vars before the Settings value — the
//! desktop-wide convention (env = explicit override), matching every other
//! desktop collector; (2) `session_reset` is emitted via chrono `to_rfc3339`
//! (`+00:00`) rather than the Mac's `Z`, the same instant and parsed identically
//! by the frontend.

use std::time::Duration;

use serde_json::Value;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const COMPUTE_URL: &str = "https://apps.abacus.ai/api/_getOrganizationComputePoints";
const BILLING_URL: &str = "https://apps.abacus.ai/api/_getBillingInfo";
const TIMEOUT: Duration = Duration::from_secs(15);
const BILLING_GRACE: Duration = Duration::from_secs(5);
const ENV_VARS: [&str; 2] = ["ABACUS_COOKIE", "ABACUS_SESSION_TOKEN"];

#[derive(Debug, Clone, Default)]
struct ComputePoints {
    total: f64,
    left: f64,
}

#[derive(Debug, Clone, Default)]
struct Billing {
    next_billing: Option<String>,
    current_tier: Option<String>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let cookie = match resolve_cookie() {
        Some(c) => c,
        None => {
            log::debug!("[Abacus] no session cookie (env or Settings) — skipping");
            return Ok(None);
        }
    };
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;

    // Compute points is required; billing is best-effort, bounded to 5s.
    let (compute_res, billing_res) = tokio::join!(
        fetch_compute(&client, &cookie),
        fetch_billing(&client, &cookie),
    );
    let compute = compute_res?;
    let billing = billing_res.ok().unwrap_or_default();
    Ok(Some(build_snapshot(&compute, &billing)))
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
        .and_then(|c| c.abacus_cookie)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_compute(
    client: &reqwest::Client,
    cookie: &str,
) -> Result<ComputePoints, CollectorError> {
    let result = fetch_result(client, COMPUTE_URL, false, cookie, None).await?;
    parse_compute(&result)
}

async fn fetch_billing(client: &reqwest::Client, cookie: &str) -> Result<Billing, CollectorError> {
    let result = fetch_result(client, BILLING_URL, true, cookie, Some(BILLING_GRACE)).await?;
    Ok(parse_billing(&result))
}

/// Performs the request and validates the `{success, result}` envelope,
/// returning the inner `result` object.
async fn fetch_result(
    client: &reqwest::Client,
    url: &str,
    post: bool,
    cookie: &str,
    timeout: Option<Duration>,
) -> Result<Value, CollectorError> {
    let mut req = if post {
        client.post(url).body("{}")
    } else {
        client.get(url)
    };
    req = req
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("Cookie", cookie)
        .header("Origin", "https://apps.abacus.ai")
        .header("Referer", "https://apps.abacus.ai/");
    if let Some(t) = timeout {
        req = req.timeout(t);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| CollectorError::Http(format!("request: {e}")))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| CollectorError::Http(format!("body: {e}")))?;
    unwrap_result(status, &body)
}

/// Auth-flavored error detection (tightened — no bare "session"/"expired" — to
/// avoid false positives on non-auth error strings).
fn is_auth_error(message: &str) -> bool {
    let m = message.to_lowercase();
    [
        "unauthorized",
        "unauthenticated",
        "not authenticated",
        "forbidden",
        "authenticate",
        "log in",
        "login",
        "session expired",
        "expired session",
        "invalid session",
        "no session",
    ]
    .iter()
    .any(|n| m.contains(n))
}

fn unwrap_result(status: u16, body: &str) -> Result<Value, CollectorError> {
    if status == 401 || status == 403 {
        return Err(CollectorError::Http(
            "session expired or unauthorized".to_string(),
        ));
    }
    if status != 200 {
        return Err(CollectorError::Http(format!("HTTP {status}")));
    }
    let root: Value = serde_json::from_str(body)
        .map_err(|e| CollectorError::SchemaOrIo(format!("parse: {e}")))?;
    if root.get("success").and_then(Value::as_bool) == Some(true) {
        if let Some(result) = root.get("result").filter(|r| r.is_object()) {
            return Ok(result.clone());
        }
    }
    let message = root
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    if is_auth_error(message) {
        Err(CollectorError::Http(format!("auth: {message}")))
    } else {
        Err(CollectorError::SchemaOrIo(message.to_string()))
    }
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

fn parse_compute(result: &Value) -> Result<ComputePoints, CollectorError> {
    let total = num(result.get("totalComputePoints"));
    let left = num(result.get("computePointsLeft"));
    match (total, left) {
        (Some(total), Some(left)) => Ok(ComputePoints { total, left }),
        _ => Err(CollectorError::SchemaOrIo(
            "missing compute-point fields".to_string(),
        )),
    }
}

fn parse_billing(result: &Value) -> Billing {
    Billing {
        next_billing: result
            .get("nextBillingDate")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|d| d.with_timezone(&chrono::Utc).to_rfc3339())
            }),
        current_tier: result
            .get("currentTier")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
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

/// Title-case each word for the billing tier — first alphanumeric of each
/// whitespace/punctuation-separated word uppercased, so realistic tiers like
/// `"pro-plus"` → `"Pro-Plus"` and `"free"` → `"Free"`. This approximates Swift
/// `.capitalized` for the tier strings Abacus actually returns; it does NOT
/// replicate Foundation's exact ICU word-boundary quirks (apostrophes,
/// digit↔letter transitions), which real tier names don't exercise.
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

fn build_snapshot(compute: &ComputePoints, billing: &Billing) -> QuotaSnapshot {
    let plan_type = billing
        .current_tier
        .as_deref()
        .map(capitalize_words)
        .unwrap_or_else(|| "Account".to_string());
    let reset = billing.next_billing.clone();

    let total = compute.total.max(0.0);
    let left_clamped = compute.left.max(0.0).min(total);

    if total <= 0.0 {
        // Degenerate: no usable cap ⇒ status-only.
        return QuotaSnapshot {
            status_text: Some(format!(
                "{} compute points",
                credit_count(compute.left.max(0.0))
            )),
            plan_type,
            remaining: 0,
            quota: 0,
            session_reset: reset,
            tiers: Vec::new(),
        };
    }

    let quota = total.round() as i64;
    let remaining = (left_clamped.round() as i64).clamp(0, quota);
    QuotaSnapshot {
        status_text: Some(format!(
            "{}/{} compute points",
            group_int(remaining),
            credit_count(total)
        )),
        plan_type,
        remaining,
        quota,
        session_reset: reset.clone(),
        tiers: vec![TierEntry {
            name: "Compute Points".to_string(),
            quota,
            remaining,
            reset_time: reset,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_points_map_to_quota_with_billing_tier() {
        let compute = parse_compute(
            &serde_json::json!({"totalComputePoints": 100000, "computePointsLeft": 42500}),
        )
        .unwrap();
        let billing = parse_billing(&serde_json::json!({
            "currentTier": "pro-plus",
            "nextBillingDate": "2026-08-01T00:00:00Z"
        }));
        let snap = build_snapshot(&compute, &billing);
        assert_eq!(snap.quota, 100_000);
        assert_eq!(snap.remaining, 42_500);
        assert_eq!(snap.plan_type, "Pro-Plus");
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Compute Points");
        assert_eq!(
            snap.session_reset.as_deref(),
            Some("2026-08-01T00:00:00+00:00")
        );
        assert_eq!(
            snap.status_text.as_deref(),
            Some("42,500/100,000 compute points")
        );
    }

    #[test]
    fn left_is_clamped_to_total() {
        let compute = ComputePoints {
            total: 100.0,
            left: 250.0,
        };
        let snap = build_snapshot(&compute, &Billing::default());
        assert_eq!(snap.remaining, 100); // clamped to quota
        assert_eq!(snap.plan_type, "Account"); // no tier
    }

    #[test]
    fn degenerate_total_is_status_only() {
        let compute = ComputePoints {
            total: 0.0,
            left: 500.0,
        };
        let snap = build_snapshot(&compute, &Billing::default());
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.status_text.as_deref(), Some("500 compute points"));
    }

    #[test]
    fn unwrap_success_envelope_returns_result() {
        let body = r#"{"success":true,"result":{"totalComputePoints":10}}"#;
        let result = unwrap_result(200, body).unwrap();
        assert_eq!(num(result.get("totalComputePoints")), Some(10.0));
    }

    #[test]
    fn unwrap_auth_status_and_message() {
        assert!(matches!(
            unwrap_result(401, "{}"),
            Err(CollectorError::Http(_))
        ));
        // failure envelope with an auth-flavored message.
        let body = r#"{"success":false,"error":"Please log in to continue"}"#;
        assert!(matches!(
            unwrap_result(200, body),
            Err(CollectorError::Http(_))
        ));
        // failure envelope with a non-auth message → schema error, not re-auth.
        let body = r#"{"success":false,"error":"internal model error"}"#;
        assert!(matches!(
            unwrap_result(200, body),
            Err(CollectorError::SchemaOrIo(_))
        ));
    }

    #[test]
    fn missing_compute_fields_rejected() {
        assert!(matches!(
            parse_compute(&serde_json::json!({"other": 1})),
            Err(CollectorError::SchemaOrIo(_))
        ));
    }
}
