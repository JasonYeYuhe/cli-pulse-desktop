//! GLM (Zhipu AI / 智谱) status collection — port of macOS `GLMCollector`.
//!
//! Endpoint: `GET https://open.bigmodel.cn/api/paas/v4/models` (env
//! `GLM_API_HOST` swaps the host). Auth: `Authorization: Bearer <apiKey>` —
//! env `GLM_API_KEY` / `ZHIPU_API_KEY` / `CHATGLM_API_KEY`, or the
//! Settings-stored `glm_api_key`.
//!
//! GLM has no usable quota/limit endpoint, so this is a **status-only**
//! provider (like the Mac's `.statusOnly`): the models-list endpoint is a
//! connectivity probe. There is no numeric gauge — the result is a
//! `status_text` line only (`"N models available"` / `"X.XX CUR remaining"`
//! when a balance shape happens to come back / `"Connected"`), rendered on the
//! provider card via the local `status_text` channel. `quota`/`remaining` are
//! 0 and `tiers` empty so no misleading bar is drawn.

use std::time::Duration;

use serde_json::Value;

use super::{CollectorError, QuotaSnapshot};

const DEFAULT_HOST: &str = "open.bigmodel.cn";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq)]
enum GlmUsage {
    /// A credit balance shape (`data.balance` or `result.balance`).
    Balance { amount: f64, currency: String },
    /// The models-list shape — a connectivity probe (count of models).
    Models(usize),
    /// Authenticated, but no balance or models parsed.
    Connected,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[GLM] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_models(&token).await?;
    let usage = parse_response(&body)?;
    Ok(Some(map_to_snapshot(&usage)))
}

fn resolve_key() -> Option<String> {
    for env in ["GLM_API_KEY", "ZHIPU_API_KEY", "CHATGLM_API_KEY"] {
        if let Ok(k) = std::env::var(env) {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.glm_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn models_url() -> String {
    let host = std::env::var("GLM_API_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.to_string());
    format!("https://{host}/api/paas/v4/models")
}

async fn fetch_models(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(models_url())
        .header("Authorization", format!("Bearer {token}"))
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

/// Flexible parse (verbatim from the Mac's shape-detection order): a balance
/// object, then a models array, then a `result.balance`, else Connected.
fn parse_response(body: &str) -> Result<GlmUsage, CollectorError> {
    let json: Value =
        serde_json::from_str(body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;

    // Shape 1: { "data": { "balance": <num>, "currency": "CNY" } }
    if let Some(bal) = balance_at(&json, "data") {
        return Ok(bal);
    }
    // Shape 2: { "data": [ {..models..} ] } → count as a connectivity probe.
    if let Some(arr) = json.get("data").and_then(Value::as_array) {
        return Ok(GlmUsage::Models(arr.len()));
    }
    // Shape 3: { "result": { "balance": <num>, "currency": "CNY" } }
    if let Some(bal) = balance_at(&json, "result") {
        return Ok(bal);
    }
    Ok(GlmUsage::Connected)
}

fn balance_at(json: &Value, key: &str) -> Option<GlmUsage> {
    let obj = json.get(key)?;
    let amount = obj.get("balance").and_then(Value::as_f64)?;
    let currency = obj
        .get("currency")
        .and_then(Value::as_str)
        .unwrap_or("CNY")
        .to_string();
    Some(GlmUsage::Balance { amount, currency })
}

fn map_to_snapshot(u: &GlmUsage) -> QuotaSnapshot {
    let status_text = match u {
        GlmUsage::Balance { amount, currency } => {
            format!("{:.2} {} remaining", amount.max(0.0), currency)
        }
        GlmUsage::Models(n) if *n > 0 => format!("{n} models available"),
        _ => "Connected".to_string(),
    };
    // Status-only: no numeric gauge — the status_text line carries the meaning.
    QuotaSnapshot {
        status_text: Some(status_text),
        plan_type: String::new(),
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
    fn balance_shape_under_data() {
        let u = parse_response(r#"{"data":{"balance":50.0,"currency":"CNY"}}"#).unwrap();
        assert_eq!(
            u,
            GlmUsage::Balance {
                amount: 50.0,
                currency: "CNY".to_string()
            }
        );
        let snap = map_to_snapshot(&u);
        assert_eq!(snap.status_text.as_deref(), Some("50.00 CNY remaining"));
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
    }

    #[test]
    fn models_list_shape_counts() {
        let u =
            parse_response(r#"{"data":[{"id":"glm-4"},{"id":"glm-4v"},{"id":"glm-3"}]}"#).unwrap();
        assert_eq!(u, GlmUsage::Models(3));
        assert_eq!(
            map_to_snapshot(&u).status_text.as_deref(),
            Some("3 models available")
        );
    }

    #[test]
    fn result_wrapped_balance_and_default_currency() {
        // balance under "result", currency missing → default CNY.
        let u = parse_response(r#"{"result":{"balance":12.5}}"#).unwrap();
        assert_eq!(
            u,
            GlmUsage::Balance {
                amount: 12.5,
                currency: "CNY".to_string()
            }
        );
    }

    #[test]
    fn empty_models_or_unknown_is_connected() {
        assert_eq!(
            parse_response(r#"{"data":[]}"#).unwrap(),
            GlmUsage::Models(0)
        );
        assert_eq!(
            map_to_snapshot(&GlmUsage::Models(0)).status_text.as_deref(),
            Some("Connected")
        );
        assert_eq!(
            parse_response(r#"{"unrelated":1}"#).unwrap(),
            GlmUsage::Connected
        );
        assert_eq!(
            map_to_snapshot(&GlmUsage::Connected).status_text.as_deref(),
            Some("Connected")
        );
    }

    #[test]
    fn models_url_default_and_host_override() {
        // env-free assertion — default host in this test binary.
        assert!(models_url().ends_with("/api/paas/v4/models"));
        assert!(models_url().starts_with("https://"));
    }
}
