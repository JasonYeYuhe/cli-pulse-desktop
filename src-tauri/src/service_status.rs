//! Provider service-status (incident/health) — port of the Mac
//! `ServiceStatus.swift` (badge scope: top-level Atlassian Statuspage v2
//! `status.json` only, not the full component tree).
//!
//! Answers "is it me or is the provider down?" — surfaces a colored dot on a
//! provider card when the provider itself has a published incident. Public
//! endpoints, **no auth**; every failure degrades to "no status" (graceful,
//! never an error the user sees). The parse core is unit-tested without a
//! network; the fetch wrapper is intentionally thin.

use std::time::Duration;

use once_cell::sync::Lazy;
use serde::Serialize;
use tokio::sync::Mutex;

/// Severity normalized from the Statuspage v2 `status.indicator` string.
/// Higher = worse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusIndicator {
    Operational, // statuspage "none"
    Maintenance,
    Minor,
    Major,
    Critical,
    Unknown, // unrecognized / missing
}

impl StatusIndicator {
    fn from_statuspage(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "none" => Self::Operational,
            "maintenance" => Self::Maintenance,
            "minor" => Self::Minor,
            "major" => Self::Major,
            "critical" => Self::Critical,
            _ => Self::Unknown,
        }
    }
}

/// A point-in-time read of one provider's published service status.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub provider: String,
    pub indicator: StatusIndicator,
    /// Human description, e.g. "All Systems Operational".
    pub description: String,
    /// The human status page URL (for a click-through), if published.
    pub page_url: Option<String>,
}

/// Provider display-name → Statuspage host. Subset of the Mac catalog covering
/// the desktop's shipped providers; Gemini + OpenRouter have no standard
/// Atlassian Statuspage, so they surface no badge (honest, not an error).
fn status_host(provider: &str) -> Option<&'static str> {
    match provider {
        "Claude" => Some("status.claude.com"),
        "Codex" => Some("status.openai.com"),
        "Cursor" => Some("status.cursor.com"),
        "Copilot" => Some("www.githubstatus.com"),
        _ => None,
    }
}

/// Tolerantly parse a Statuspage v2 `status.json` body. `None` only when the
/// body isn't a JSON object with a `status.indicator` string; a missing
/// description/url degrades to ""/None rather than failing the whole parse.
fn parse_status(provider: &str, body: &str) -> Option<ServiceStatus> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let indicator_raw = v.get("status")?.get("indicator")?.as_str()?;
    Some(ServiceStatus {
        provider: provider.to_string(),
        indicator: StatusIndicator::from_statuspage(indicator_raw),
        description: v
            .get("status")
            .and_then(|s| s.get("description"))
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string(),
        page_url: v
            .get("page")
            .and_then(|p| p.get("url"))
            .and_then(|u| u.as_str())
            .map(String::from),
    })
}

async fn fetch_one(provider: &str) -> Option<ServiceStatus> {
    let host = status_host(provider)?;
    let url = format!("https://{host}/api/v2/status.json");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().await.ok()?;
    parse_status(provider, &body)
}

// 5-minute TTL cache so re-opening the Providers tab doesn't re-hit four
// status hosts every time (no burst, no global timer — lazy per fetch).
type StatusCache = Option<(std::time::Instant, Vec<ServiceStatus>)>;
static CACHE: Lazy<Mutex<StatusCache>> = Lazy::new(|| Mutex::new(None));
const TTL: Duration = Duration::from_secs(300);

/// Fetch all supported providers' statuses concurrently (cached ~5min).
/// Providers whose fetch/parse fails are simply omitted.
pub async fn get_statuses() -> Vec<ServiceStatus> {
    {
        let g = CACHE.lock().await;
        if let Some((at, cached)) = g.as_ref() {
            if at.elapsed() < TTL {
                return cached.clone();
            }
        }
    }
    // Four fixed providers → a plain `join!` (concurrent on one task, no spawn).
    let (a, b, c, d) = tokio::join!(
        fetch_one("Claude"),
        fetch_one("Codex"),
        fetch_one("Cursor"),
        fetch_one("Copilot"),
    );
    let out: Vec<ServiceStatus> = [a, b, c, d].into_iter().flatten().collect();
    {
        let mut g = CACHE.lock().await;
        *g = Some((std::time::Instant::now(), out.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indicator_maps_statuspage_values() {
        assert_eq!(
            StatusIndicator::from_statuspage("none"),
            StatusIndicator::Operational
        );
        assert_eq!(
            StatusIndicator::from_statuspage("MINOR"),
            StatusIndicator::Minor
        );
        assert_eq!(
            StatusIndicator::from_statuspage(" major "),
            StatusIndicator::Major
        );
        assert_eq!(
            StatusIndicator::from_statuspage("critical"),
            StatusIndicator::Critical
        );
        assert_eq!(
            StatusIndicator::from_statuspage("maintenance"),
            StatusIndicator::Maintenance
        );
        assert_eq!(
            StatusIndicator::from_statuspage("wat"),
            StatusIndicator::Unknown
        );
    }

    #[test]
    fn catalog_covers_shipped_and_omits_pageless() {
        assert_eq!(status_host("Claude"), Some("status.claude.com"));
        assert_eq!(status_host("Codex"), Some("status.openai.com"));
        assert_eq!(status_host("Cursor"), Some("status.cursor.com"));
        assert_eq!(status_host("Copilot"), Some("www.githubstatus.com"));
        // No standard Statuspage → no badge (not an error).
        assert_eq!(status_host("Gemini"), None);
        assert_eq!(status_host("OpenRouter"), None);
        // Every provider we concurrently fetch (`get_statuses`) has a catalog
        // entry.
        for p in ["Claude", "Codex", "Cursor", "Copilot"] {
            assert!(status_host(p).is_some());
        }
    }

    #[test]
    fn parse_operational_and_incident() {
        let ok = r#"{"page":{"name":"Claude","url":"https://status.claude.com","updated_at":"2026-07-06T00:00:00Z"},"status":{"indicator":"none","description":"All Systems Operational"}}"#;
        let s = parse_status("Claude", ok).unwrap();
        assert_eq!(s.indicator, StatusIndicator::Operational);
        assert_eq!(s.description, "All Systems Operational");
        assert_eq!(s.page_url.as_deref(), Some("https://status.claude.com"));

        let bad = r#"{"status":{"indicator":"major","description":"Partial Outage"}}"#;
        let s = parse_status("Codex", bad).unwrap();
        assert_eq!(s.indicator, StatusIndicator::Major);
        assert_eq!(s.description, "Partial Outage");
        assert!(s.page_url.is_none()); // no page.url → None, not a parse failure
    }

    #[test]
    fn parse_rejects_non_statuspage_bodies() {
        assert!(parse_status("Claude", "not json").is_none());
        assert!(parse_status("Claude", r#"{"foo":1}"#).is_none()); // no status.indicator
        assert!(parse_status("Claude", r#"{"status":{}}"#).is_none());
    }
}
