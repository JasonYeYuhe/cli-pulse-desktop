//! StepFun (阶跃星辰) usage collection — port of macOS `StepFunCollector`
//! (itself derived from steipete/CodexBar, MIT).
//!
//! `POST platform.stepfun.com/api/…Dashboard/QueryStepPlanRateLimit` (+ a
//! best-effort `GetStepPlanStatus` for the plan name). Plain-JSON connect-RPC
//! (empty `{}` body), NOT protobuf. Auth: the **`Oasis-Token` cookie** from env
//! `STEPFUN_COOKIE` / `STEPFUN_OASIS_TOKEN`, or the Settings-stored
//! `stepfun_cookie`. Manual paste only.
//!
//! CRITICAL: upstream can password-login; we NEVER do (autonomy/security
//! contract) — the standalone `Oasis-Token` cookie drives the usage RPCs
//! directly. `Oasis-Webid` is read from the cookie or falls back to a vendored
//! constant.
//!
//! A real percent-window `.quota` collector (like Claude/Codex): 5-hour +
//! weekly windows, each a *left-rate* in 0…1 → `remaining = round(rate·100)`
//! (`quota = 100`). The 5-hour window is the headline.

use std::time::Duration;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const RATE_LIMIT_URL: &str =
    "https://platform.stepfun.com/api/step.openapi.devcenter.Dashboard/QueryStepPlanRateLimit";
const PLAN_STATUS_URL: &str =
    "https://platform.stepfun.com/api/step.openapi.devcenter.Dashboard/GetStepPlanStatus";
// Vendored device/app constants (CodexBar).
const FALLBACK_WEBID: &str = "c8a1002d2c457e758785a9979832217c7c0b884c";
const APP_ID: &str = "10300";
const TIMEOUT: Duration = Duration::from_secs(15);
const PLAN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default, Deserialize)]
struct RateLimitResponse {
    #[serde(default)]
    status: Option<i64>,
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: Option<String>,
    #[serde(
        default,
        rename = "five_hour_usage_left_rate",
        deserialize_with = "de_flex_num"
    )]
    five_hour_left: Option<f64>,
    #[serde(
        default,
        rename = "weekly_usage_left_rate",
        deserialize_with = "de_flex_num"
    )]
    weekly_left: Option<f64>,
    #[serde(
        default,
        rename = "five_hour_usage_reset_time",
        deserialize_with = "de_flex_ts"
    )]
    five_hour_reset: Option<i64>,
    #[serde(
        default,
        rename = "weekly_usage_reset_time",
        deserialize_with = "de_flex_ts"
    )]
    weekly_reset: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PlanStatusResponse {
    #[serde(default)]
    subscription: Option<PlanSubscription>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PlanSubscription {
    #[serde(default)]
    name: Option<String>,
}

/// Decodes from a JSON int OR float (StepFun returns either for rates).
fn de_flex_num<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let v: Option<Value> = Option::deserialize(d)?;
    Ok(v.and_then(|v| match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }))
}

/// Decodes from a JSON string OR int (StepFun returns epoch as either).
fn de_flex_ts<'de, D>(d: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let v: Option<Value> = Option::deserialize(d)?;
    Ok(v.and_then(|v| match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }))
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let header = match resolve_cookie() {
        Some(c) => c,
        None => {
            log::debug!("[StepFun] no session cookie (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let (token, webid_from_cookie) = cookie_tokens(&header);
    let token = match token {
        Some(t) => t,
        None => {
            log::debug!("[StepFun] cookie has no Oasis-Token — skipping");
            return Ok(None);
        }
    };
    let webid = webid_from_cookie.unwrap_or_else(|| FALLBACK_WEBID.to_string());

    let rate_body = post_rpc(RATE_LIMIT_URL, &token, &webid, TIMEOUT).await?;
    let rate = parse_rate_limit(&rate_body)?;

    // Best-effort plan name — grace-bounded, never sinks the rate data.
    let plan = match post_rpc(PLAN_STATUS_URL, &token, &webid, PLAN_GRACE).await {
        Ok(body) => parse_plan_status(&body),
        Err(_) => None,
    };

    Ok(Some(map_to_snapshot(&rate, plan)))
}

fn resolve_cookie() -> Option<String> {
    for env in ["STEPFUN_COOKIE", "STEPFUN_OASIS_TOKEN"] {
        if let Ok(c) = std::env::var(env) {
            let c = c.trim().to_string();
            if !c.is_empty() {
                return Some(c);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.stepfun_cookie)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Extracts `Oasis-Token` + `Oasis-Webid` from a Cookie header, or accepts a
/// bare token (env with no `=`/`;`).
fn cookie_tokens(raw: &str) -> (Option<String>, Option<String>) {
    let mut token = None;
    let mut webid = None;
    for part in raw.split(';') {
        let mut it = part.splitn(2, '=');
        let (Some(name), Some(value)) = (it.next(), it.next()) else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if name.eq_ignore_ascii_case("Oasis-Token") {
            token = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("Oasis-Webid") {
            webid = Some(value.to_string());
        }
    }
    if token.is_none() {
        let t = raw.trim();
        if !t.is_empty() && !t.contains('=') && !t.contains(';') {
            token = Some(t.to_string());
        }
    }
    (token, webid)
}

async fn post_rpc(
    url: &str,
    token: &str,
    webid: &str,
    timeout: Duration,
) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .post(url)
        .body("{}")
        .header("content-type", "application/json")
        .header("oasis-appid", APP_ID)
        .header("oasis-platform", "web")
        .header("oasis-webid", webid)
        .header(
            "Cookie",
            format!("Oasis-Token={token}; Oasis-Webid={webid}"),
        )
        .header(
            "user-agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36",
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

struct Rate {
    five_hour_left: f64,
    weekly_left: f64,
    five_hour_reset: i64,
    weekly_reset: i64,
}

fn parse_rate_limit(body: &str) -> Result<Rate, CollectorError> {
    let resp: RateLimitResponse =
        serde_json::from_str(body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    if resp.status != Some(1) {
        let detail = resp
            .message
            .or_else(|| resp.code.map(|c| c.to_string()))
            .unwrap_or_else(|| "not ok".to_string());
        return Err(CollectorError::SchemaOrIo(format!("StepFun: {detail}")));
    }
    match (
        resp.five_hour_left,
        resp.weekly_left,
        resp.five_hour_reset,
        resp.weekly_reset,
    ) {
        (Some(fh), Some(wk), Some(fhr), Some(wkr)) => Ok(Rate {
            five_hour_left: fh,
            weekly_left: wk,
            five_hour_reset: fhr,
            weekly_reset: wkr,
        }),
        _ => Err(CollectorError::SchemaOrIo(
            "StepFun: missing usage/reset fields".to_string(),
        )),
    }
}

fn parse_plan_status(body: &str) -> Option<String> {
    serde_json::from_str::<PlanStatusResponse>(body)
        .ok()
        .and_then(|r| r.subscription)
        .and_then(|s| s.name)
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
}

fn remaining_percent(left_rate: f64) -> i64 {
    (left_rate.clamp(0.0, 1.0) * 100.0).round() as i64
}

fn reset_iso(secs: i64) -> Option<String> {
    if secs <= 0 {
        return None;
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0).map(|d| d.to_rfc3339())
}

fn map_to_snapshot(rate: &Rate, plan: Option<String>) -> QuotaSnapshot {
    let fh = remaining_percent(rate.five_hour_left);
    let wk = remaining_percent(rate.weekly_left);
    let fh_reset = reset_iso(rate.five_hour_reset);
    let wk_reset = reset_iso(rate.weekly_reset);

    let tiers = vec![
        TierEntry {
            name: "5-hour".to_string(),
            quota: 100,
            remaining: fh,
            reset_time: fh_reset.clone(),
        },
        TierEntry {
            name: "Weekly".to_string(),
            quota: 100,
            remaining: wk,
            reset_time: wk_reset,
        },
    ];

    QuotaSnapshot {
        status_text: None,
        // Headline = the 5-hour window.
        plan_type: plan.unwrap_or_else(|| "StepFun".to_string()),
        remaining: fh,
        quota: 100,
        session_reset: fh_reset,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_tokens_extracts_token_and_webid() {
        let (t, w) = cookie_tokens("Oasis-Token=abc123; Oasis-Webid=web789; other=x");
        assert_eq!(t.as_deref(), Some("abc123"));
        assert_eq!(w.as_deref(), Some("web789"));
        // Case-insensitive name match.
        let (t, _) = cookie_tokens("oasis-token=XYZ");
        assert_eq!(t.as_deref(), Some("XYZ"));
    }

    #[test]
    fn cookie_tokens_accepts_bare_token() {
        let (t, w) = cookie_tokens("bareTokenValue");
        assert_eq!(t.as_deref(), Some("bareTokenValue"));
        assert!(w.is_none());
        // A cookie header with '=' but no Oasis-Token → no token.
        let (t, _) = cookie_tokens("session=foo; other=bar");
        assert!(t.is_none());
    }

    #[test]
    fn parses_rate_limit_flexible_number_and_timestamp() {
        // rates as float, resets as string epoch.
        let r = parse_rate_limit(
            r#"{"status":1,"five_hour_usage_left_rate":0.7,"weekly_usage_left_rate":0.42,
                "five_hour_usage_reset_time":"1800000000","weekly_usage_reset_time":1800600000}"#,
        )
        .unwrap();
        assert!((r.five_hour_left - 0.7).abs() < 1e-9);
        assert_eq!(r.five_hour_reset, 1_800_000_000);
        assert_eq!(r.weekly_reset, 1_800_600_000);
        // rates as int (1 / 0) also decode.
        let r = parse_rate_limit(
            r#"{"status":1,"five_hour_usage_left_rate":1,"weekly_usage_left_rate":0,
                "five_hour_usage_reset_time":10,"weekly_usage_reset_time":20}"#,
        )
        .unwrap();
        assert_eq!(r.five_hour_left, 1.0);
        assert_eq!(r.weekly_left, 0.0);
    }

    #[test]
    fn rate_limit_gate_and_missing_fields_error() {
        // status != 1 → error with message.
        assert!(parse_rate_limit(r#"{"status":0,"message":"nope"}"#).is_err());
        // status ok but missing fields → error.
        assert!(parse_rate_limit(r#"{"status":1,"five_hour_usage_left_rate":0.5}"#).is_err());
    }

    #[test]
    fn maps_windows_to_percent_gauges() {
        let rate = Rate {
            five_hour_left: 0.7,
            weekly_left: 0.42,
            five_hour_reset: 1_800_000_000,
            weekly_reset: 1_800_600_000,
        };
        let snap = map_to_snapshot(&rate, Some("Pro".to_string()));
        assert_eq!(snap.quota, 100);
        assert_eq!(snap.remaining, 70); // 5-hour headline
        assert_eq!(snap.plan_type, "Pro");
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "5-hour");
        assert_eq!(snap.tiers[0].remaining, 70);
        assert_eq!(snap.tiers[1].name, "Weekly");
        assert_eq!(snap.tiers[1].remaining, 42);
        assert!(snap.session_reset.is_some());
        // No plan → default name.
        let snap = map_to_snapshot(&rate, None);
        assert_eq!(snap.plan_type, "StepFun");
    }

    #[test]
    fn remaining_percent_clamps_left_rate() {
        assert_eq!(remaining_percent(0.7), 70);
        assert_eq!(remaining_percent(1.5), 100); // API glitch > 1 → clamped
        assert_eq!(remaining_percent(-0.2), 0);
    }

    #[test]
    fn plan_status_parse_and_empty() {
        assert_eq!(
            parse_plan_status(r#"{"subscription":{"name":"  Growth  "}}"#).as_deref(),
            Some("Growth")
        );
        assert!(parse_plan_status(r#"{"subscription":{"name":""}}"#).is_none());
        assert!(parse_plan_status(r#"{}"#).is_none());
    }
}
