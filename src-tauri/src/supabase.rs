//! Supabase REST client — generic RPC + typed wrappers for the CLI Pulse
//! backend surface.
//!
//! Auth model mirrors the Python helper and Swift APIClient: anon key in
//! `apikey` + `Authorization: Bearer` headers; mutating RPCs validate
//! `(device_id, helper_secret)` internally via SECURITY DEFINER functions
//! defined in `backend/supabase/helper_rpc.sql` of the main repo.
//!
//! We do NOT persist the anon user's access token here because the helper
//! flow never authenticates as a user — it authenticates as a paired
//! device. User-scoped reads (dashboard_summary etc.) will be added in
//! Sprint 2 when we wire the iPhone-originated session token path.

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::creds;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum SupabaseError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },

    #[error("Supabase RPC returned error code `{code}`: {message}")]
    Rpc { code: String, message: String },

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type SupabaseResult<T> = Result<T, SupabaseError>;

fn client() -> SupabaseResult<Client> {
    let c = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("cli-pulse-desktop/0.1")
        .build()?;
    Ok(c)
}

/// Generic RPC POST. Returns the parsed JSON value. The caller is
/// responsible for turning business-logic errors (HTTP 200 with
/// `{"error": "...", "message": "..."}` body, as used by
/// `register_helper`) into [`SupabaseError::Rpc`] via [`check_rpc_error`].
pub async fn rpc<P: Serialize>(name: &str, params: &P) -> SupabaseResult<Value> {
    let url = format!("{}/rest/v1/rpc/{}", creds::supabase_url(), name);
    let anon = creds::supabase_anon_key();
    let resp = client()?
        .post(&url)
        .header("apikey", &anon)
        .header("Authorization", format!("Bearer {anon}"))
        .header("Content-Type", "application/json")
        .json(params)
        .send()
        .await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    if !(200..300).contains(&status) {
        return Err(SupabaseError::Http { status, body: text });
    }
    if text.is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_str(&text)?)
}

/// Many CLI Pulse RPCs (register_helper notably) return HTTP 200 but
/// encode expected failures (rate_limited / invalid_code / expired) as
/// `{"error": "code", "message": "..."}`. Callers should pipe through
/// this helper to convert to a typed error.
pub fn check_rpc_error(v: &Value) -> SupabaseResult<()> {
    if let Some(obj) = v.as_object() {
        if let Some(code) = obj.get("error").and_then(|x| x.as_str()) {
            let message = obj
                .get("message")
                .and_then(|x| x.as_str())
                .unwrap_or("no message")
                .to_string();
            return Err(SupabaseError::Rpc {
                code: code.to_string(),
                message,
            });
        }
    }
    Ok(())
}

// ========================================================================
// register_helper
// ========================================================================

#[derive(Debug, Clone, Serialize)]
pub struct RegisterHelperRequest<'a> {
    pub p_pairing_code: &'a str,
    pub p_device_name: &'a str,
    pub p_device_type: &'a str,
    pub p_system: &'a str,
    pub p_helper_version: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegisterHelperResponse {
    pub device_id: String,
    pub user_id: String,
    pub helper_secret: String,
}

pub async fn register_helper(
    req: &RegisterHelperRequest<'_>,
) -> SupabaseResult<RegisterHelperResponse> {
    let v = rpc("register_helper", req).await?;
    check_rpc_error(&v)?;
    Ok(serde_json::from_value(v)?)
}

// ========================================================================
// helper_sync (minimal — Sprint 1 sends empty sessions/alerts/quotas,
// just to validate the round-trip. Real collectors land in Sprint 2.)
// ========================================================================

#[derive(Debug, Clone, Serialize)]
pub struct HelperSyncRequest<'a> {
    pub p_device_id: &'a str,
    pub p_helper_secret: &'a str,
    pub p_sessions: Value,
    pub p_alerts: Value,
    pub p_provider_remaining: Value,
    pub p_provider_tiers: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HelperSyncResponse {
    #[serde(default)]
    pub sessions_synced: i64,
    #[serde(default)]
    pub alerts_synced: i64,
}

pub async fn helper_sync(req: &HelperSyncRequest<'_>) -> SupabaseResult<HelperSyncResponse> {
    let v = rpc("helper_sync", req).await?;
    check_rpc_error(&v)?;
    Ok(serde_json::from_value(v)?)
}

// ========================================================================
// (Removed in v0.2.14) upsert_daily_usage — the RPC requires auth.uid()
// but Tauri callers only have the helper's anon-key credentials, so the
// call would always fail and surface as a sync error. Per-device daily
// usage metrics return in v0.3.1 via a multi-device-aware path.
// ========================================================================

// ========================================================================
// helper_heartbeat
// ========================================================================

#[derive(Debug, Clone, Serialize)]
pub struct HelperHeartbeatRequest<'a> {
    pub p_device_id: &'a str,
    pub p_helper_secret: &'a str,
    pub p_cpu_usage: i32,
    pub p_memory_usage: i32,
    pub p_active_session_count: i32,
}

pub async fn helper_heartbeat(req: &HelperHeartbeatRequest<'_>) -> SupabaseResult<()> {
    let v = rpc("helper_heartbeat", req).await?;
    check_rpc_error(&v)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_rpc_error_passes_through_non_errors() {
        let ok = json!({"device_id": "abc", "helper_secret": "def"});
        assert!(check_rpc_error(&ok).is_ok());
    }

    #[test]
    fn check_rpc_error_extracts_typed_error() {
        let bad = json!({"error": "invalid_code", "message": "Invalid pairing code"});
        let err = check_rpc_error(&bad).unwrap_err();
        match err {
            SupabaseError::Rpc { code, message } => {
                assert_eq!(code, "invalid_code");
                assert_eq!(message, "Invalid pairing code");
            }
            _ => panic!("wrong error variant"),
        }
    }
}
