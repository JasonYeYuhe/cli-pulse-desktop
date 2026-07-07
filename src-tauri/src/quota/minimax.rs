//! MiniMax coding-plan quota collection — port of macOS `MiniMaxCollector`.
//!
//! Endpoint: `GET https://api.minimax.io/v1/coding_plan/remains`
//! (env `MINIMAX_HOST` swaps the host; `MINIMAX_REMAINS_URL` overrides the
//! whole URL).
//! Auth: `Authorization: Bearer <apiKey>` — env `MINIMAX_API_KEY` or the
//! Settings-stored `minimax_api_key`.
//!
//! Only the **API-token** path is ported. The Mac collector also has a
//! cookie-auth fallback, but its own docs call cookie auth "less reliable
//! (MiniMax often returns HTTP 1004 with cookies alone)" and the desktop
//! credential model is api-key-based — so the token path (the preferred,
//! most-stable method) is the one we carry over.
//!
//! Response (flat): `{ "model_remains": int, "total": int,
//! "end_time": "..."|"remains_time": "..." }`. The coding plan is a single
//! depleting quota (`quota = total`, `remaining = model_remains`), with the
//! reset timestamp passed through verbatim (`end_time`, falling back to
//! `remains_time` — matching the Mac's priority).

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const DEFAULT_HOST: &str = "api.minimax.io";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct MiniMaxRemains {
    #[serde(default)]
    model_remains: i64,
    #[serde(default)]
    total: i64,
    // Mac coalesces `end_time ?? remains_time` (end_time wins). Keep both
    // fields so that priority is preserved even when both are present.
    #[serde(default)]
    end_time: Option<String>,
    #[serde(default)]
    remains_time: Option<String>,
}

impl MiniMaxRemains {
    fn reset(&self) -> Option<String> {
        self.end_time
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| self.remains_time.clone().filter(|s| !s.is_empty()))
    }
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[MiniMax] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_remains(&token).await?;
    let parsed: MiniMaxRemains =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    Ok(Some(map_to_snapshot(&parsed)))
}

fn resolve_key() -> Option<String> {
    if let Ok(k) = std::env::var("MINIMAX_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.minimax_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn remains_url() -> String {
    if let Ok(url) = std::env::var("MINIMAX_REMAINS_URL") {
        if !url.trim().is_empty() {
            return url.trim().to_string();
        }
    }
    let host = std::env::var("MINIMAX_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.to_string());
    format!("https://{host}/v1/coding_plan/remains")
}

async fn fetch_remains(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(remains_url())
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        // Mirror the Mac's source tag verbatim — it's a server-side client
        // attribution header; matching the proven value avoids any
        // source-based rate-limit/allow-list surprise.
        .header("MM-API-Source", "CLIPulseBar")
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

fn map_to_snapshot(m: &MiniMaxRemains) -> QuotaSnapshot {
    let total = m.total.max(0);
    // Floor remaining at 0 (a negative gauge is meaningless); unlike Crof we
    // do NOT clamp to `total` — the Mac passes `model_remains` through as-is.
    let remaining = m.model_remains.max(0);
    let reset = m.reset();

    let mut tiers: Vec<TierEntry> = Vec::new();
    if total > 0 {
        tiers.push(TierEntry {
            name: "Coding Plan".to_string(),
            quota: total,
            remaining,
            reset_time: reset.clone(),
        });
    }

    QuotaSnapshot {
        status_text: None,
        plan_type: "Coding Plan".to_string(),
        remaining,
        quota: total,
        session_reset: reset,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remains_to_coding_plan_tier() {
        let m: MiniMaxRemains = serde_json::from_str(
            r#"{"model_remains":700,"total":1000,"end_time":"2026-07-10T00:00:00Z"}"#,
        )
        .unwrap();
        let snap = map_to_snapshot(&m);
        assert_eq!(snap.plan_type, "Coding Plan");
        assert_eq!(snap.quota, 1000);
        assert_eq!(snap.remaining, 700);
        assert_eq!(snap.session_reset.as_deref(), Some("2026-07-10T00:00:00Z"));
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Coding Plan");
        assert_eq!(snap.tiers[0].quota, 1000);
        assert_eq!(snap.tiers[0].remaining, 700);
        assert_eq!(
            snap.tiers[0].reset_time.as_deref(),
            Some("2026-07-10T00:00:00Z")
        );
    }

    #[test]
    fn end_time_wins_over_remains_time_and_alias_fallback() {
        // Both present → end_time priority (matches Mac coalesce order).
        let m: MiniMaxRemains = serde_json::from_str(
            r#"{"total":10,"model_remains":5,"end_time":"E","remains_time":"R"}"#,
        )
        .unwrap();
        assert_eq!(map_to_snapshot(&m).session_reset.as_deref(), Some("E"));
        // Only remains_time → falls back to it.
        let m: MiniMaxRemains =
            serde_json::from_str(r#"{"total":10,"model_remains":5,"remains_time":"R"}"#).unwrap();
        assert_eq!(map_to_snapshot(&m).session_reset.as_deref(), Some("R"));
        // Empty end_time string is ignored in favor of remains_time.
        let m: MiniMaxRemains = serde_json::from_str(
            r#"{"total":10,"model_remains":5,"end_time":"","remains_time":"R"}"#,
        )
        .unwrap();
        assert_eq!(map_to_snapshot(&m).session_reset.as_deref(), Some("R"));
    }

    #[test]
    fn no_total_yields_no_tier() {
        let m: MiniMaxRemains = serde_json::from_str(r#"{"model_remains":0,"total":0}"#).unwrap();
        let snap = map_to_snapshot(&m);
        assert_eq!(snap.quota, 0);
        assert_eq!(snap.remaining, 0);
        assert!(snap.tiers.is_empty());
        assert!(snap.session_reset.is_none());
    }

    #[test]
    fn defensive_defaults_and_negative_floor() {
        // Empty object → all zero, no tiers, plan_type still set.
        let m: MiniMaxRemains = serde_json::from_str(r#"{}"#).unwrap();
        let snap = map_to_snapshot(&m);
        assert_eq!(snap.plan_type, "Coding Plan");
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        // Negative counts floor at 0.
        let m: MiniMaxRemains = serde_json::from_str(r#"{"total":-3,"model_remains":-9}"#).unwrap();
        let snap = map_to_snapshot(&m);
        assert_eq!(snap.quota, 0);
        assert_eq!(snap.remaining, 0);
    }

    #[test]
    fn remains_url_default_path() {
        // env-free assertion — default host, no env set in this test binary.
        assert!(remains_url().starts_with("https://"));
        assert!(remains_url().ends_with("/v1/coding_plan/remains"));
    }
}
