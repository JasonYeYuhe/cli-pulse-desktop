//! Warp usage collection — port of macOS `WarpCollector`.
//!
//! Endpoint: `POST https://app.warp.dev/graphql/v2?op=GetRequestLimitInfo`
//! Auth: `Authorization: Bearer <apiKey>` — env `WARP_API_KEY` / `WARP_TOKEN`,
//! or the Settings-stored `warp_api_key`.
//!
//! GraphQL, but ordinary from our side: a fixed query string + empty
//! `requestContext` in a JSON POST body, and a nested JSON response
//! (`data.user.user.{requestLimitInfo,bonusGrants}`). A real depleting `.quota`
//! (raw request counts): the request allocation (`requestLimit` /
//! `requestsUsedSinceLastRefresh`) is the headline; bonus grants are a second
//! tier. Unlimited plans have no numeric gauge — surfaced via `plan_type`.

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const GRAPHQL_URL: &str = "https://app.warp.dev/graphql/v2?op=GetRequestLimitInfo";
const QUERY: &str = "query GetRequestLimitInfo($requestContext:RequestContext!){user(requestContext:$requestContext){user{requestLimitInfo{isUnlimited nextRefreshTime requestLimit requestsUsedSinceLastRefresh}bonusGrants{requestCreditsGranted requestCreditsRemaining expiration}}}}";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct GraphQlResponse {
    #[serde(default)]
    data: DataField,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DataField {
    #[serde(default)]
    user: UserOuter,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct UserOuter {
    #[serde(default)]
    user: UserInner,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct UserInner {
    #[serde(default, rename = "requestLimitInfo")]
    request_limit_info: Option<RequestLimitInfo>,
    #[serde(default, rename = "bonusGrants")]
    bonus_grants: Vec<BonusGrant>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RequestLimitInfo {
    #[serde(default, rename = "isUnlimited")]
    is_unlimited: bool,
    #[serde(default, rename = "requestLimit")]
    request_limit: i64,
    #[serde(default, rename = "requestsUsedSinceLastRefresh")]
    requests_used: i64,
    #[serde(default, rename = "nextRefreshTime")]
    next_refresh_time: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct BonusGrant {
    #[serde(default, rename = "requestCreditsGranted")]
    granted: i64,
    #[serde(default, rename = "requestCreditsRemaining")]
    remaining: i64,
    #[serde(default)]
    expiration: Option<String>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[Warp] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_graphql(&token).await?;
    let parsed: GraphQlResponse =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    let inner = parsed.data.user.user;
    let rli = inner.request_limit_info.ok_or_else(|| {
        CollectorError::SchemaOrIo("Warp: unexpected response structure".to_string())
    })?;
    Ok(Some(map_to_snapshot(&rli, &inner.bonus_grants)))
}

fn resolve_key() -> Option<String> {
    for env in ["WARP_API_KEY", "WARP_TOKEN"] {
        if let Ok(k) = std::env::var(env) {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.warp_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_graphql(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let payload = serde_json::json!({
        "query": QUERY,
        "variables": { "requestContext": {} },
    });
    let resp = client
        .post(GRAPHQL_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", "Warp/1.0")
        .body(payload.to_string())
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

fn map_to_snapshot(rli: &RequestLimitInfo, grants: &[BonusGrant]) -> QuotaSnapshot {
    let mut tiers: Vec<TierEntry> = Vec::new();

    let limit = rli.request_limit.max(0);
    let used = rli.requests_used.max(0);
    if !rli.is_unlimited && limit > 0 {
        tiers.push(TierEntry {
            name: "Requests".to_string(),
            quota: limit,
            remaining: (limit - used).max(0),
            reset_time: rli.next_refresh_time.clone(),
        });
    }

    // Aggregate bonus grants (sum granted/remaining; first non-null expiration).
    let bonus_granted: i64 = grants.iter().map(|g| g.granted.max(0)).sum();
    let bonus_remaining: i64 = grants.iter().map(|g| g.remaining.max(0)).sum();
    let bonus_expiration = grants.iter().find_map(|g| g.expiration.clone());
    if bonus_granted > 0 {
        tiers.push(TierEntry {
            name: "Bonus Credits".to_string(),
            quota: bonus_granted,
            remaining: bonus_remaining,
            reset_time: bonus_expiration,
        });
    }

    // Unlimited plans have no numeric gauge (Mac uses a nil gauge) → 0/0 with
    // the plan name carrying the meaning.
    let (quota, remaining) = if rli.is_unlimited {
        (0, 0)
    } else {
        (limit, (limit - used).max(0))
    };

    QuotaSnapshot {
        status_text: None,
        plan_type: if rli.is_unlimited {
            "Unlimited".to_string()
        } else {
            "Free".to_string()
        },
        remaining,
        quota,
        session_reset: rli.next_refresh_time.clone(),
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> (RequestLimitInfo, Vec<BonusGrant>) {
        let r: GraphQlResponse = serde_json::from_str(json).unwrap();
        let inner = r.data.user.user;
        (inner.request_limit_info.unwrap(), inner.bonus_grants)
    }

    const FREE: &str = r#"{"data":{"user":{"user":{
        "requestLimitInfo":{"isUnlimited":false,"requestLimit":1000,"requestsUsedSinceLastRefresh":300,"nextRefreshTime":"2026-08-01T00:00:00Z"},
        "bonusGrants":[{"requestCreditsGranted":50,"requestCreditsRemaining":20,"expiration":"2026-09-01T00:00:00Z"}]
    }}}}"#;

    #[test]
    fn free_plan_maps_requests_and_bonus_tiers() {
        let (rli, grants) = parse(FREE);
        let snap = map_to_snapshot(&rli, &grants);
        assert_eq!(snap.plan_type, "Free");
        assert_eq!(snap.quota, 1000);
        assert_eq!(snap.remaining, 700); // 1000 - 300
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "Requests");
        assert_eq!(snap.tiers[0].quota, 1000);
        assert_eq!(snap.tiers[0].remaining, 700);
        assert_eq!(
            snap.tiers[0].reset_time.as_deref(),
            Some("2026-08-01T00:00:00Z")
        );
        assert_eq!(snap.tiers[1].name, "Bonus Credits");
        assert_eq!(snap.tiers[1].quota, 50);
        assert_eq!(snap.tiers[1].remaining, 20);
    }

    #[test]
    fn unlimited_plan_has_no_gauge_but_keeps_bonus() {
        let (rli, grants) = parse(
            r#"{"data":{"user":{"user":{
                "requestLimitInfo":{"isUnlimited":true,"requestLimit":0,"requestsUsedSinceLastRefresh":0},
                "bonusGrants":[{"requestCreditsGranted":10,"requestCreditsRemaining":10}]
            }}}}"#,
        );
        let snap = map_to_snapshot(&rli, &grants);
        assert_eq!(snap.plan_type, "Unlimited");
        assert_eq!(snap.quota, 0);
        assert_eq!(snap.remaining, 0);
        // No Requests tier when unlimited; Bonus still shown.
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Bonus Credits");
    }

    #[test]
    fn bonus_grants_aggregate_and_first_expiration() {
        let (rli, grants) = parse(
            r#"{"data":{"user":{"user":{
                "requestLimitInfo":{"isUnlimited":false,"requestLimit":100,"requestsUsedSinceLastRefresh":0},
                "bonusGrants":[
                    {"requestCreditsGranted":30,"requestCreditsRemaining":10,"expiration":"E1"},
                    {"requestCreditsGranted":20,"requestCreditsRemaining":5,"expiration":"E2"}
                ]
            }}}}"#,
        );
        let snap = map_to_snapshot(&rli, &grants);
        let bonus = snap
            .tiers
            .iter()
            .find(|t| t.name == "Bonus Credits")
            .unwrap();
        assert_eq!(bonus.quota, 50); // 30 + 20
        assert_eq!(bonus.remaining, 15); // 10 + 5
        assert_eq!(bonus.reset_time.as_deref(), Some("E1")); // first expiration
    }

    #[test]
    fn used_over_limit_floors_remaining_and_no_bonus() {
        let (rli, grants) = parse(
            r#"{"data":{"user":{"user":{
                "requestLimitInfo":{"isUnlimited":false,"requestLimit":100,"requestsUsedSinceLastRefresh":150}
            }}}}"#,
        );
        let snap = map_to_snapshot(&rli, &grants);
        assert_eq!(snap.remaining, 0); // max(0, 100 - 150)
        assert_eq!(snap.tiers.len(), 1); // no bonus grants
        assert_eq!(snap.tiers[0].name, "Requests");
    }

    #[test]
    fn missing_request_limit_info_is_error() {
        let r: GraphQlResponse =
            serde_json::from_str(r#"{"data":{"user":{"user":{"bonusGrants":[]}}}}"#).unwrap();
        assert!(r.data.user.user.request_limit_info.is_none());
    }
}
