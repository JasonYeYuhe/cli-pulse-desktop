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
    rpc_with_auth(name, params, None).await
}

/// Generic RPC POST with optional user-JWT authorization. When
/// `user_jwt` is `Some`, the `Authorization: Bearer <jwt>` header
/// carries the user's session token instead of the anon key — required
/// by RPCs whose `auth.uid()` check needs to resolve to the user
/// (e.g. `register_desktop_helper`). The `apikey` header still carries
/// the anon key (Supabase's rate-limit + project gate).
pub async fn rpc_with_auth<P: Serialize>(
    name: &str,
    params: &P,
    user_jwt: Option<&str>,
) -> SupabaseResult<Value> {
    let url = format!("{}/rest/v1/rpc/{}", creds::supabase_url(), name);
    let anon = creds::supabase_anon_key();
    let bearer = user_jwt.unwrap_or(&anon);
    let resp = client()?
        .post(&url)
        .header("apikey", &anon)
        .header("Authorization", format!("Bearer {bearer}"))
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
// register_desktop_helper (v0.3.0) — auth.uid()-based mirror used by
// the desktop OTP sign-in flow. Same response shape as register_helper
// so the calling code path past this RPC is identical.
// ========================================================================

#[derive(Debug, Clone, Serialize)]
pub struct RegisterDesktopHelperRequest<'a> {
    pub p_device_name: &'a str,
    pub p_device_type: &'a str,
    pub p_system: &'a str,
    pub p_helper_version: &'a str,
}

pub async fn register_desktop_helper(
    req: &RegisterDesktopHelperRequest<'_>,
    user_jwt: &str,
) -> SupabaseResult<RegisterHelperResponse> {
    let v = rpc_with_auth("register_desktop_helper", req, Some(user_jwt)).await?;
    check_rpc_error(&v)?;
    Ok(serde_json::from_value(v)?)
}

// ========================================================================
// device_status (v0.3.0) — used by the helper_sync error classifier
// (see lib.rs::auth_account_check). Returns one of:
//   "healthy"          — keep syncing
//   "device_missing"   — device row deleted; clear local pairing
//   "account_missing"  — account deleted; clear local pairing
// ========================================================================

#[derive(Debug, Clone, Serialize)]
pub struct DeviceStatusRequest<'a> {
    pub p_device_id: &'a str,
    pub p_helper_secret: &'a str,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum DeviceStatus {
    Healthy,
    DeviceMissing,
    AccountMissing,
}

pub async fn device_status(req: &DeviceStatusRequest<'_>) -> SupabaseResult<DeviceStatus> {
    let v = rpc("device_status", req).await?;
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
// helper_sync_daily_usage (v0.3.1) — sibling RPC to helper_sync that
// handles multi-device daily-usage upload via the helper credential
// path. server-side derives user_id from (p_device_id, p_helper_secret)
// match, so we cannot spoof another device. v0.2.14 had removed this
// path; v0.3.1 reintroduces it through this dedicated RPC instead of
// extending helper_sync (avoids touching the live helper_sync body).
// ========================================================================

#[derive(Debug, Clone, Serialize)]
pub struct DailyUsageMetric {
    pub metric_date: String,
    pub provider: String,
    pub model: String,
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
}

impl DailyUsageMetric {
    pub fn from_entry(e: &crate::scanner::DailyEntry) -> Option<Self> {
        // Swift APIClient explicitly filters out the `__claude_msg__` bucket
        // so the server doesn't see a synthetic model row.
        if e.model == crate::scanner::CLAUDE_MSG_BUCKET_MODEL {
            return None;
        }
        Some(Self {
            metric_date: e.date.clone(),
            provider: e.provider.clone(),
            model: e.model.clone(),
            input_tokens: e.input_tokens,
            cached_tokens: e.cached_tokens,
            output_tokens: e.output_tokens,
            cost: e.cost_usd.unwrap_or(0.0),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HelperSyncDailyUsageRequest<'a> {
    pub p_device_id: &'a str,
    pub p_helper_secret: &'a str,
    pub p_metrics: Vec<DailyUsageMetric>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HelperSyncDailyUsageResponse {
    #[serde(default)]
    pub metrics_synced: i64,
    #[serde(default)]
    pub metrics_errored: i64,
}

pub async fn helper_sync_daily_usage(
    req: &HelperSyncDailyUsageRequest<'_>,
) -> SupabaseResult<HelperSyncDailyUsageResponse> {
    let v = rpc("helper_sync_daily_usage", req).await?;
    check_rpc_error(&v)?;
    Ok(serde_json::from_value(v)?)
}

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
