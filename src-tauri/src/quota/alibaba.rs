//! Alibaba coding-plan quota — port of macOS `AlibabaCollector`.
//!
//! Endpoint: `POST {host}/data/api.json?action=…queryCodingPlanInstanceInfoV2…`
//! Auth: Bearer token (also mirrored into `x-api-key` + `X-DashScope-API-Key`
//! headers, as the Mac does) from env `ALIBABA_CODING_PLAN_API_KEY` or the
//! Settings `alibaba_api_key`.
//!
//! **Region routing:** try the International console
//! (`modelstudio.console.alibabacloud.com`, commodity `broadscope-bailian-intl`)
//! first, fall back to China mainland (`bailian.console.aliyun.com`, commodity
//! `broadscope-bailian`) on any failure — mirrors the Mac do/catch.
//!
//! Real depleting `.quota`: up to three windows (5-hour / weekly / monthly),
//! each `used`/`total` with a next-refresh time. The 5-hour window (else
//! monthly) drives the headline gauge; `status_text` shows "used/total used".

use std::time::Duration;

use serde_json::{json, Value};

use super::{CollectorError, QuotaSnapshot, TierEntry};

const HOST_INTL: &str = "https://modelstudio.console.alibabacloud.com";
const HOST_CN: &str = "https://bailian.console.aliyun.com";
const COMMODITY_INTL: &str = "broadscope-bailian-intl";
const COMMODITY_CN: &str = "broadscope-bailian";
const API_PATH: &str = "/data/api.json?action=zeldaEasy.broadscope-bailian.codingPlan.queryCodingPlanInstanceInfoV2&product=broadscope-bailian&api=queryCodingPlanInstanceInfoV2";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default)]
struct AlibabaQuota {
    plan_name: Option<String>,
    five_hour_used: i64,
    five_hour_total: i64,
    five_hour_reset: Option<String>,
    weekly_used: i64,
    weekly_total: i64,
    weekly_reset: Option<String>,
    monthly_used: i64,
    monthly_total: i64,
    monthly_reset: Option<String>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_token() {
        Some(t) => t,
        None => {
            log::debug!("[Alibaba] no API key (env or Settings) — skipping");
            return Ok(None);
        }
    };
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;

    // International first, China mainland on any failure.
    match fetch_quota(&client, &token, HOST_INTL, COMMODITY_INTL).await {
        Ok(body) => Ok(Some(map_to_snapshot(&parse_response(&body)?))),
        Err(intl_err) => {
            log::debug!("[Alibaba] intl endpoint failed ({intl_err:?}); trying China mainland");
            let body = fetch_quota(&client, &token, HOST_CN, COMMODITY_CN).await?;
            Ok(Some(map_to_snapshot(&parse_response(&body)?)))
        }
    }
}

fn resolve_token() -> Option<String> {
    if let Ok(k) = std::env::var("ALIBABA_CODING_PLAN_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.alibaba_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_quota(
    client: &reqwest::Client,
    token: &str,
    host: &str,
    commodity: &str,
) -> Result<String, CollectorError> {
    let body = json!({
        "queryCodingPlanInstanceInfoRequest": { "commodityCode": commodity }
    });
    let resp = client
        .post(format!("{host}{API_PATH}"))
        .header("Authorization", format!("Bearer {token}"))
        .header("x-api-key", token)
        .header("X-DashScope-API-Key", token)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
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

/// Integer field tolerant of JSON int or float encodings.
fn int_at(v: &Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn parse_response(body: &str) -> Result<AlibabaQuota, CollectorError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| CollectorError::SchemaOrIo(format!("parse: {e}")))?;
    let first = v
        .pointer("/data/codingPlanInstanceInfos")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| CollectorError::SchemaOrIo("no codingPlanInstanceInfos".into()))?;

    let plan_name = str_at(first, "planName").or_else(|| str_at(first, "packageName"));
    let q = first
        .get("codingPlanQuotaInfo")
        .cloned()
        .unwrap_or(json!({}));

    Ok(AlibabaQuota {
        plan_name,
        five_hour_used: int_at(&q, "per5HourUsedQuota"),
        five_hour_total: int_at(&q, "per5HourTotalQuota"),
        five_hour_reset: str_at(&q, "per5HourQuotaNextRefreshTime"),
        weekly_used: int_at(&q, "perWeekUsedQuota"),
        weekly_total: int_at(&q, "perWeekTotalQuota"),
        weekly_reset: str_at(&q, "perWeekQuotaNextRefreshTime"),
        monthly_used: int_at(&q, "perBillMonthUsedQuota"),
        monthly_total: int_at(&q, "perBillMonthTotalQuota"),
        monthly_reset: str_at(&q, "perBillMonthQuotaNextRefreshTime"),
    })
}

fn map_to_snapshot(a: &AlibabaQuota) -> QuotaSnapshot {
    let mut tiers: Vec<TierEntry> = Vec::new();
    if a.five_hour_total > 0 {
        tiers.push(TierEntry {
            name: "5h Window".to_string(),
            quota: a.five_hour_total,
            remaining: (a.five_hour_total - a.five_hour_used).max(0),
            reset_time: a.five_hour_reset.clone(),
        });
    }
    if a.weekly_total > 0 {
        tiers.push(TierEntry {
            name: "Weekly".to_string(),
            quota: a.weekly_total,
            remaining: (a.weekly_total - a.weekly_used).max(0),
            reset_time: a.weekly_reset.clone(),
        });
    }
    if a.monthly_total > 0 {
        tiers.push(TierEntry {
            name: "Monthly".to_string(),
            quota: a.monthly_total,
            remaining: (a.monthly_total - a.monthly_used).max(0),
            reset_time: a.monthly_reset.clone(),
        });
    }

    // Headline: the 5-hour window if present, else the monthly window.
    let (used, total, reset) = if a.five_hour_total > 0 {
        (
            a.five_hour_used,
            a.five_hour_total,
            a.five_hour_reset.clone(),
        )
    } else {
        (a.monthly_used, a.monthly_total, a.monthly_reset.clone())
    };

    QuotaSnapshot {
        status_text: Some(if total > 0 {
            format!("{used}/{total} used")
        } else {
            "Unknown".to_string()
        }),
        plan_type: a
            .plan_name
            .clone()
            .unwrap_or_else(|| "Coding Plan".to_string()),
        remaining: if total > 0 { (total - used).max(0) } else { 0 },
        quota: total.max(0),
        session_reset: reset,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{"data":{"codingPlanInstanceInfos":[{
        "planName":"Coding Plan Pro",
        "codingPlanQuotaInfo":{
            "per5HourUsedQuota":30,"per5HourTotalQuota":100,
            "per5HourQuotaNextRefreshTime":"2026-07-07T15:00:00Z",
            "perWeekUsedQuota":200,"perWeekTotalQuota":1000,
            "perWeekQuotaNextRefreshTime":"2026-07-13T00:00:00Z",
            "perBillMonthUsedQuota":500,"perBillMonthTotalQuota":5000,
            "perBillMonthQuotaNextRefreshTime":"2026-08-01T00:00:00Z"
        }
    }]}}"#;

    #[test]
    fn maps_three_windows_and_five_hour_headline() {
        let snap = map_to_snapshot(&parse_response(FULL).unwrap());
        assert_eq!(snap.plan_type, "Coding Plan Pro");
        assert_eq!(snap.tiers.len(), 3);
        assert_eq!(snap.tiers[0].name, "5h Window");
        assert_eq!(snap.tiers[0].remaining, 70); // 100 - 30
        assert_eq!(snap.tiers[1].name, "Weekly");
        assert_eq!(snap.tiers[2].name, "Monthly");
        // Headline = 5-hour window.
        assert_eq!(snap.quota, 100);
        assert_eq!(snap.remaining, 70);
        assert_eq!(snap.status_text.as_deref(), Some("30/100 used"));
        assert_eq!(snap.session_reset.as_deref(), Some("2026-07-07T15:00:00Z"));
    }

    #[test]
    fn monthly_headline_when_no_five_hour_window() {
        let body = r#"{"data":{"codingPlanInstanceInfos":[{
            "packageName":"Monthly Only",
            "codingPlanQuotaInfo":{
                "perBillMonthUsedQuota":10,"perBillMonthTotalQuota":40,
                "perBillMonthQuotaNextRefreshTime":"2026-08-01T00:00:00Z"
            }
        }]}}"#;
        let snap = map_to_snapshot(&parse_response(body).unwrap());
        assert_eq!(snap.plan_type, "Monthly Only"); // packageName fallback
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Monthly");
        assert_eq!(snap.quota, 40);
        assert_eq!(snap.remaining, 30);
        assert_eq!(snap.status_text.as_deref(), Some("10/40 used"));
    }

    #[test]
    fn no_quota_info_is_unknown_status_and_default_plan() {
        let body = r#"{"data":{"codingPlanInstanceInfos":[{}]}}"#;
        let snap = map_to_snapshot(&parse_response(body).unwrap());
        assert_eq!(snap.plan_type, "Coding Plan");
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.status_text.as_deref(), Some("Unknown"));
    }

    #[test]
    fn missing_infos_array_is_schema_error() {
        assert!(matches!(
            parse_response(r#"{"data":{}}"#),
            Err(CollectorError::SchemaOrIo(_))
        ));
    }
}
