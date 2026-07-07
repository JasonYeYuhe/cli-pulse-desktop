//! Deepgram usage collection — port of macOS `DeepgramCollector` (itself
//! derived from steipete/CodexBar, MIT).
//!
//! Deepgram exposes NO quota/credits denominator — only absolute usage counts
//! (requests / audio hours / tokens / TTS chars) — so it's **status-only**:
//! "N requests · X audio hrs · N tokens".
//!
//! Auth: `Authorization: Token <key>` (the custom "Token" scheme, NOT Bearer) —
//! env `DEEPGRAM_API_KEY` or the Settings-stored `deepgram_api_key`. A pinned
//! `DEEPGRAM_PROJECT_ID` ⇒ one usage call; otherwise `GET /v1/projects` then a
//! capped (≤5), **concurrent** per-project `GET /v1/projects/{id}/usage/breakdown`,
//! aggregated. Each request is 12s-bounded; the fan-out runs concurrently so a
//! multi-project account doesn't serialize.

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot};

const BASE_URL: &str = "https://api.deepgram.com/v1";
const MAX_PROJECTS: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, Default, PartialEq)]
struct UsageAggregate {
    requests: i64,
    hours: f64,
    total_hours: f64,
    tokens_in: i64,
    tokens_out: i64,
    tts_characters: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ProjectsResponse {
    #[serde(default)]
    projects: Vec<ProjectDto>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ProjectDto {
    #[serde(default, rename = "project_id")]
    project_id: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct UsageResponse {
    #[serde(default)]
    results: Vec<UsageResult>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct UsageResult {
    #[serde(default)]
    hours: Option<f64>,
    #[serde(default, rename = "total_hours")]
    total_hours: Option<f64>,
    #[serde(default, rename = "tokens_in")]
    tokens_in: Option<i64>,
    #[serde(default, rename = "tokens_out")]
    tokens_out: Option<i64>,
    #[serde(default, rename = "tts_characters")]
    tts_characters: Option<i64>,
    #[serde(default)]
    requests: Option<i64>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[Deepgram] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;

    // Pinned project ⇒ one fast call, no list walk.
    if let Some(pid) = resolve_project_id() {
        let agg = fetch_project_usage(&client, &pid, &token).await?;
        return Ok(Some(build_snapshot(agg, None, 1)));
    }

    let projects = fetch_projects(&client, &token).await?;
    if projects.is_empty() {
        return Err(CollectorError::SchemaOrIo(
            "Deepgram: no projects for this API key".to_string(),
        ));
    }
    let capped: Vec<ProjectDto> = projects.into_iter().take(MAX_PROJECTS).collect();

    // Concurrent per-project usage fetch (NOT serialized). Spawned inside an
    // async fn running on the Tauri tokio runtime — safe.
    let mut handles = Vec::with_capacity(capped.len());
    for p in &capped {
        let client = client.clone();
        let token = token.clone();
        let pid = p.project_id.clone();
        // Spawn each per-project usage fetch concurrently.
        let fut = tokio::spawn(async move { fetch_project_usage(&client, &pid, &token).await }); // @allow tokio-spawn — async ctx, Tauri runtime entered
        handles.push(fut);
    }
    let mut parts = Vec::with_capacity(handles.len());
    for h in handles {
        let agg = h
            .await
            .map_err(|e| CollectorError::SchemaOrIo(format!("Deepgram: task join: {e}")))??;
        parts.push(agg);
    }

    let combined = combine(&parts);
    let name = if capped.len() == 1 {
        capped.first().and_then(|p| p.name.clone())
    } else {
        None
    };
    Ok(Some(build_snapshot(combined, name, capped.len())))
}

fn resolve_key() -> Option<String> {
    if let Ok(k) = std::env::var("DEEPGRAM_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.deepgram_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn resolve_project_id() -> Option<String> {
    std::env::var("DEEPGRAM_PROJECT_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn get(client: &reqwest::Client, url: &str, token: &str) -> Result<String, CollectorError> {
    let resp = client
        .get(url)
        // Deepgram uses the custom "Token" scheme (NOT Bearer).
        .header("Authorization", format!("Token {token}"))
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

async fn fetch_projects(
    client: &reqwest::Client,
    token: &str,
) -> Result<Vec<ProjectDto>, CollectorError> {
    let body = get(client, &format!("{BASE_URL}/projects"), token).await?;
    let resp: ProjectsResponse =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    Ok(resp.projects)
}

async fn fetch_project_usage(
    client: &reqwest::Client,
    project_id: &str,
    token: &str,
) -> Result<UsageAggregate, CollectorError> {
    let url = format!("{BASE_URL}/projects/{project_id}/usage/breakdown");
    let body = get(client, &url, token).await?;
    parse_usage_aggregate(&body)
}

fn parse_usage_aggregate(body: &str) -> Result<UsageAggregate, CollectorError> {
    let resp: UsageResponse =
        serde_json::from_str(body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    let mut agg = UsageAggregate::default();
    for r in &resp.results {
        agg.requests += r.requests.unwrap_or(0);
        agg.hours += r.hours.unwrap_or(0.0);
        agg.total_hours += r.total_hours.unwrap_or(0.0);
        agg.tokens_in += r.tokens_in.unwrap_or(0);
        agg.tokens_out += r.tokens_out.unwrap_or(0);
        agg.tts_characters += r.tts_characters.unwrap_or(0);
    }
    Ok(agg)
}

fn combine(parts: &[UsageAggregate]) -> UsageAggregate {
    let mut acc = UsageAggregate::default();
    for p in parts {
        acc.requests += p.requests;
        acc.hours += p.hours;
        acc.total_hours += p.total_hours;
        acc.tokens_in += p.tokens_in;
        acc.tokens_out += p.tokens_out;
        acc.tts_characters += p.tts_characters;
    }
    acc
}

/// Integer with thousands separators (e.g. `1234567` → `"1,234,567"`).
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

/// Round to 1 dp, group the integer part, drop a trailing `.0`.
fn compact_decimal(value: f64) -> String {
    let v = if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    };
    let scaled = (v * 10.0).round() as i64;
    let int_part = scaled / 10;
    let frac = scaled % 10;
    if frac == 0 {
        group_int(int_part)
    } else {
        format!("{}.{}", group_int(int_part), frac)
    }
}

fn format_status_text(a: &UsageAggregate) -> String {
    let mut parts = vec![format!("{} requests", group_int(a.requests))];
    if a.hours > 0.0 {
        parts.push(format!("{} audio hrs", compact_decimal(a.hours)));
    } else if a.total_hours > 0.0 {
        parts.push(format!("{} billable hrs", compact_decimal(a.total_hours)));
    }
    let total_tokens = a.tokens_in + a.tokens_out;
    if total_tokens > 0 {
        parts.push(format!("{} tokens", group_int(total_tokens)));
    } else if a.tts_characters > 0 {
        parts.push(format!("{} TTS chars", group_int(a.tts_characters)));
    }
    parts.join(" · ")
}

fn build_snapshot(
    agg: UsageAggregate,
    project_name: Option<String>,
    project_count: usize,
) -> QuotaSnapshot {
    let plan_type = if project_count > 1 {
        format!("{project_count} projects")
    } else {
        project_name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "API key".to_string())
    };
    // Status-only: absolute usage counts, no gauge.
    QuotaSnapshot {
        status_text: Some(format_status_text(&agg)),
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
    fn parses_projects() {
        let body = r#"{"projects":[{"project_id":"p1","name":"Prod"},{"project_id":"p2"}]}"#;
        let resp: ProjectsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(resp.projects.len(), 2);
        assert_eq!(resp.projects[0].project_id, "p1");
        assert_eq!(resp.projects[0].name.as_deref(), Some("Prod"));
    }

    #[test]
    fn parses_and_sums_usage_results() {
        let body = r#"{"results":[
            {"requests":100,"hours":1.5,"tokens_in":1000,"tokens_out":500},
            {"requests":50,"hours":0.5,"tokens_in":200,"tokens_out":300}
        ]}"#;
        let agg = parse_usage_aggregate(body).unwrap();
        assert_eq!(agg.requests, 150);
        assert!((agg.hours - 2.0).abs() < 1e-9);
        assert_eq!(agg.tokens_in, 1200);
        assert_eq!(agg.tokens_out, 800);
    }

    #[test]
    fn combine_across_projects_sums() {
        let a = UsageAggregate {
            requests: 10,
            hours: 1.0,
            tts_characters: 5,
            ..Default::default()
        };
        let b = UsageAggregate {
            requests: 20,
            total_hours: 3.0,
            ..Default::default()
        };
        let c = combine(&[a, b]);
        assert_eq!(c.requests, 30);
        assert!((c.hours - 1.0).abs() < 1e-9);
        assert!((c.total_hours - 3.0).abs() < 1e-9);
        assert_eq!(c.tts_characters, 5);
    }

    #[test]
    fn status_text_requests_hours_tokens() {
        let agg = UsageAggregate {
            requests: 1234,
            hours: 12.5,
            tokens_in: 900,
            tokens_out: 600,
            ..Default::default()
        };
        assert_eq!(
            format_status_text(&agg),
            "1,234 requests · 12.5 audio hrs · 1,500 tokens"
        );
    }

    #[test]
    fn status_text_falls_back_to_billable_hrs_and_tts_chars() {
        // No `hours` but total_hours; no tokens but TTS chars.
        let agg = UsageAggregate {
            requests: 5,
            total_hours: 2.0,
            tts_characters: 40_000,
            ..Default::default()
        };
        assert_eq!(
            format_status_text(&agg),
            "5 requests · 2 billable hrs · 40,000 TTS chars"
        );
    }

    #[test]
    fn build_snapshot_plan_type_and_status_only() {
        let agg = UsageAggregate {
            requests: 3,
            ..Default::default()
        };
        // Multi-project → "N projects".
        let snap = build_snapshot(agg.clone(), None, 3);
        assert_eq!(snap.plan_type, "3 projects");
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.status_text.as_deref(), Some("3 requests"));
        // Single named project → its name.
        let snap = build_snapshot(agg.clone(), Some("Prod".to_string()), 1);
        assert_eq!(snap.plan_type, "Prod");
        // Single unnamed → "API key".
        let snap = build_snapshot(agg, None, 1);
        assert_eq!(snap.plan_type, "API key");
    }

    #[test]
    fn compact_decimal_rounds_and_groups() {
        assert_eq!(compact_decimal(2.0), "2");
        assert_eq!(compact_decimal(12.54), "12.5");
        assert_eq!(compact_decimal(1234.0), "1,234");
    }
}
