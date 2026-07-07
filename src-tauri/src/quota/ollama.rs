//! Ollama local-server status — port of macOS `OllamaCollector`.
//!
//! Data source (LOCAL, no auth):
//!   `GET http://localhost:11434/api/tags` — installed model list
//!   `GET http://localhost:11434/api/ps`   — currently-running models
//! Override the base with `OLLAMA_HOST`.
//!
//! Ollama has no quota model, so this is **status-only**: `quota`/`remaining`
//! stay 0 and the whole signal lives in `status_text` ("N running, M
//! installed"). Uniquely among the collectors it needs **no credential** — a
//! developer running Ollama gets it zero-config, and if Ollama isn't listening
//! the collector simply returns `Ok(None)` (the provider is absent, not an
//! error). The Mac's raw `AF_INET` socket pre-probe is replaced with a plain
//! short-timeout `reqwest` GET whose connection error is the "not running"
//! signal.

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot};

const DEFAULT_HOST: &str = "http://localhost:11434";
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default, Deserialize)]
struct ModelEntry {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ModelList {
    #[serde(default)]
    models: Vec<ModelEntry>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let base = std::env::var("OLLAMA_HOST")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.to_string());

    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;

    // /api/tags is the availability signal: a connection error means Ollama
    // isn't running locally, which is a normal absent state, not a failure.
    let tags_resp = match client.get(format!("{base}/api/tags")).send().await {
        Ok(r) => r,
        Err(e) => {
            log::debug!("[Ollama] not reachable at {base} ({e}) — skipping");
            return Ok(None);
        }
    };
    if !tags_resp.status().is_success() {
        return Err(CollectorError::Http(format!(
            "HTTP {} from /api/tags",
            tags_resp.status().as_u16()
        )));
    }
    let tags_body = tags_resp
        .text()
        .await
        .map_err(|e| CollectorError::Http(format!("tags body: {e}")))?;
    let installed = parse_names(&tags_body)
        .map_err(|e| CollectorError::SchemaOrIo(format!("tags parse: {e}")))?;

    // /api/ps is best-effort — a failure just means "0 running".
    let running = match client.get(format!("{base}/api/ps")).send().await {
        Ok(r) if r.status().is_success() => r
            .text()
            .await
            .ok()
            .and_then(|b| parse_names(&b).ok())
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    Ok(Some(build_snapshot(installed.len(), running.len())))
}

fn parse_names(body: &str) -> Result<Vec<String>, serde_json::Error> {
    let list: ModelList = serde_json::from_str(body)?;
    Ok(list
        .models
        .into_iter()
        .filter_map(|m| m.name)
        .filter(|n| !n.is_empty())
        .collect())
}

fn build_snapshot(installed: usize, running: usize) -> QuotaSnapshot {
    let status_text = if running == 0 {
        format!("{installed} models installed")
    } else {
        format!("{running} running, {installed} installed")
    };
    QuotaSnapshot {
        status_text: Some(status_text),
        plan_type: "Local".to_string(),
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
    fn parses_model_names_and_skips_unnamed() {
        let names = parse_names(
            r#"{"models":[{"name":"llama3:8b","size":123},{"size":9},{"name":"qwen2.5:7b"}]}"#,
        )
        .unwrap();
        assert_eq!(names, vec!["llama3:8b", "qwen2.5:7b"]);
    }

    #[test]
    fn missing_models_key_is_empty_not_error() {
        assert!(parse_names(r#"{}"#).unwrap().is_empty());
    }

    #[test]
    fn status_text_running_and_idle_forms() {
        assert_eq!(
            build_snapshot(4, 0).status_text.as_deref(),
            Some("4 models installed")
        );
        let s = build_snapshot(4, 2);
        assert_eq!(s.status_text.as_deref(), Some("2 running, 4 installed"));
        assert_eq!(s.plan_type, "Local");
        assert_eq!(s.quota, 0);
        assert_eq!(s.remaining, 0);
        assert!(s.tiers.is_empty());
    }
}
