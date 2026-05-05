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
// unregister_desktop_helper (v0.3.4) — server-side unpair. Deletes the
// device row gated on (device_id, helper_secret) match. See spec
// PROJECT_DEV_PLAN_2026-05-02_v0.3.4_dashboard_parity.md §4.1.
//
// Privacy: returns the same `{deleted: false, reason: 'not_found'}`
// shape for both genuinely-missing and hash-mismatch — callers without
// a valid secret cannot enumerate device UUIDs.
//
// Recomputes profiles.paired from post-DELETE count in the same
// RPC tx so a multi-device account whose Laptop A unregisters while
// Laptop B is still active does not get its paired flag flipped to
// false (Codex review fix).
// ========================================================================

#[derive(Debug, Clone, Serialize)]
pub struct UnregisterDesktopHelperRequest<'a> {
    pub p_device_id: &'a str,
    pub p_helper_secret: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnregisterDesktopHelperResponse {
    #[serde(default)]
    pub deleted: bool,
    /// Number of devices remaining for this user after the call. Only
    /// meaningful when `deleted` is true.
    #[serde(default)]
    pub remaining_devices: i64,
    /// Present when `deleted` is false; "not_found" for both the
    /// genuinely-missing and hash-mismatch cases.
    pub reason: Option<String>,
}

pub async fn unregister_desktop_helper(
    req: &UnregisterDesktopHelperRequest<'_>,
) -> SupabaseResult<UnregisterDesktopHelperResponse> {
    let v = rpc("unregister_desktop_helper", req).await?;
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
// Dashboard read RPCs (v0.3.4) — user-JWT-authenticated. Caller passes
// the access_token via rpc_with_auth. Shapes mirror what iOS / Android
// already consume from the same RPCs in app_rpc.sql.
// ========================================================================

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DashboardSummary {
    #[serde(default)]
    pub today_usage: i64,
    #[serde(default)]
    pub today_cost: f64,
    #[serde(default)]
    pub active_sessions: i64,
    #[serde(default)]
    pub online_devices: i64,
    #[serde(default)]
    pub unresolved_alerts: i64,
    #[serde(default)]
    pub today_sessions: i64,
}

pub async fn dashboard_summary(user_jwt: &str) -> SupabaseResult<DashboardSummary> {
    let v = rpc_with_auth("dashboard_summary", &serde_json::json!({}), Some(user_jwt)).await?;
    check_rpc_error(&v)?;
    Ok(serde_json::from_value(v)?)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderTier {
    pub name: String,
    #[serde(default)]
    pub quota: i64,
    #[serde(default)]
    pub remaining: i64,
    #[serde(default)]
    pub reset_time: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderSummaryRow {
    pub provider: String,
    #[serde(default)]
    pub today_usage: i64,
    #[serde(default)]
    pub total_usage: i64,
    #[serde(default)]
    pub estimated_cost: f64,
    #[serde(default)]
    pub estimated_cost_today: f64,
    #[serde(default)]
    pub estimated_cost_30_day: f64,
    #[serde(default)]
    pub remaining: Option<i64>,
    #[serde(default)]
    pub quota: Option<i64>,
    #[serde(default)]
    pub plan_type: Option<String>,
    #[serde(default)]
    pub reset_time: Option<String>,
    #[serde(default)]
    pub tiers: Vec<ProviderTier>,
    /// v0.4.15 — RFC3339 timestamp the server-side provider_quotas row
    /// was last written. `None` for synthetic rows that come purely
    /// from `usage_agg` (provider has daily_usage_metrics but never
    /// uploaded a quota snapshot — the FULL OUTER JOIN includes them).
    #[serde(default)]
    pub updated_at: Option<String>,
}

pub async fn provider_summary(user_jwt: &str) -> SupabaseResult<Vec<ProviderSummaryRow>> {
    let v = rpc_with_auth("provider_summary", &serde_json::json!({}), Some(user_jwt)).await?;
    check_rpc_error(&v)?;
    // provider_summary returns a JSON array directly (not an object).
    Ok(serde_json::from_value(v)?)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DailyUsageRow {
    pub metric_date: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub cached_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize)]
struct GetDailyUsageRequest {
    days: i64,
}

pub async fn get_daily_usage(days: u32, user_jwt: &str) -> SupabaseResult<Vec<DailyUsageRow>> {
    let req = GetDailyUsageRequest { days: days as i64 };
    let v = rpc_with_auth("get_daily_usage", &req, Some(user_jwt)).await?;
    check_rpc_error(&v)?;
    Ok(serde_json::from_value(v)?)
}

// ========================================================================
// sessions table — direct PostgREST GET (v0.5.2)
// ========================================================================
//
// Powers the v0.5.2 TopProjectsCard. Mac sibling reads
// DashboardSummary.top_projects, which the desktop server-side RPC
// doesn't return (per pre-flight schema dump 2026-05-05); the
// `daily_usage_metrics` table also has no `project` column (cost is
// keyed on provider+model+date). The only place project attribution
// lives is the `sessions` table, which has `project`,
// `estimated_cost`, `requests`, and `last_active_at`.
//
// Rather than add a new RPC (autonomy contract requires user
// approval for backend schema changes), we GET the rows directly via
// PostgREST and aggregate client-side in Rust. Caps the fetch at
// 1000 rows ordered by `last_active_at` desc — typical user has
// far fewer 30-day sessions than that, and the cap prevents a
// pathological account from pulling the whole table.

/// Single row from the `sessions` table needed for TopProjects
/// aggregation. Uses `Option<>` for fields that the server may
/// have null (the table allows null `project` for sessions whose
/// project couldn't be resolved on the device side; null
/// `estimated_cost` for sessions that haven't reported cost yet).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SessionRow {
    pub project: Option<String>,
    pub estimated_cost: Option<f64>,
    #[serde(default)]
    pub requests: Option<i64>,
    pub last_active_at: String,
    #[serde(default)]
    pub total_usage: Option<i64>,
}

/// Fetch `sessions` rows for the given user with `last_active_at >= since`.
/// Direct PostgREST GET (not via RPC). Server-side filtering on
/// user_id (RLS-enforced) + last_active_at gte. Returns up to
/// `LIMIT` rows ordered by `estimated_cost.desc.nullslast` so the
/// truncation tail (cheap or zero-cost sessions) carries the
/// least signal for the top-projects aggregate. Per Gemini 3.1 Pro
/// v0.5.2 review P1: the original `last_active_at.desc` ordering
/// would bias truncation away from cost, causing pathological
/// accounts with 5 k+ sessions to undercount older expensive
/// projects. Aggregation happens in
/// `top_projects::aggregate_top_projects`.
pub async fn get_sessions_since(
    user_id: &str,
    since: chrono::DateTime<chrono::Utc>,
    user_jwt: &str,
) -> SupabaseResult<Vec<SessionRow>> {
    // 5 000-row cap covers the 99.9-percentile heavy user. Below
    // that, ordering by cost desc means truncation hits only the
    // long-tail cheap sessions whose buckets the top-5 view never
    // surfaces anyway.
    const LIMIT: u32 = 5000;
    let url = format!("{}/rest/v1/sessions", creds::supabase_url());
    let anon = creds::supabase_anon_key();
    let resp = client()?
        .get(&url)
        .header("apikey", &anon)
        .header("Authorization", format!("Bearer {user_jwt}"))
        .query(&[
            ("user_id", format!("eq.{user_id}")),
            ("last_active_at", format!("gte.{}", since.to_rfc3339())),
            (
                "select",
                "project,estimated_cost,requests,last_active_at,total_usage".to_string(),
            ),
            ("order", "estimated_cost.desc.nullslast".to_string()),
            ("limit", LIMIT.to_string()),
        ])
        .send()
        .await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    if !(200..300).contains(&status) {
        return Err(SupabaseError::Http { status, body: text });
    }
    if text.is_empty() {
        return Ok(vec![]);
    }
    Ok(serde_json::from_str(&text)?)
}

// ========================================================================
// alerts table — direct PostgREST GET (v0.5.3)
// ========================================================================
//
// Powers the v0.5.3 RiskSignalsCard data-source switch. Previously
// the card consumed `preview_alerts` output (client-computed alerts
// from local scan + thresholds), which produced a confusing
// divergence with the Overview tile's `unresolved_alerts` count
// (the tile reads server-stored alerts via `dashboard_summary`,
// not the client-computed set). v0.5.3 unifies the card's data
// source with the tile's by reading the same `alerts` table.
//
// RLS pre-flight (Supabase MCP, 2026-05-05): `alerts` table has
// RLS enabled with policy "Users can manage own alerts" using
// `(auth.uid() = user_id)` for ALL operations — same posture as
// `sessions`. Authenticated user JWT scopes naturally to their
// own rows.
//
// LIMIT 200 is generous — typical user has < 20 unresolved alerts
// at any time. The card only renders top-3-by-severity; the rest
// flow into the "+N more" overflow indicator. If a heavy account
// exceeds 200 unresolved, we'd undercount the overflow but the
// top-3 are still correct.

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerAlert {
    pub id: String,
    #[serde(rename = "type")]
    pub alert_type: String,
    pub severity: String, // "Info" / "Warning" / "Critical"
    pub title: String,
    pub message: Option<String>,
    pub created_at: String,
    pub related_project_id: Option<String>,
    pub related_project_name: Option<String>,
    pub related_session_id: Option<String>,
    pub related_session_name: Option<String>,
    pub related_provider: Option<String>,
    pub related_device_name: Option<String>,
    pub is_read: Option<bool>,
    pub is_resolved: Option<bool>,
}

/// Fetch unresolved alerts for the given user. Direct PostgREST GET
/// (server-side filter on `is_resolved=eq.false` + RLS-enforced
/// `user_id`). Ordered by `created_at.desc` so the most recent
/// alert is always in the top-of-card position.
pub async fn get_unresolved_alerts(
    user_id: &str,
    user_jwt: &str,
) -> SupabaseResult<Vec<ServerAlert>> {
    const LIMIT: u32 = 200;
    let url = format!("{}/rest/v1/alerts", creds::supabase_url());
    let anon = creds::supabase_anon_key();
    let resp = client()?
        .get(&url)
        .header("apikey", &anon)
        .header("Authorization", format!("Bearer {user_jwt}"))
        .query(&[
            ("user_id", format!("eq.{user_id}")),
            ("is_resolved", "eq.false".to_string()),
            (
                "select",
                "id,type,severity,title,message,created_at,related_project_id,\
                 related_project_name,related_session_id,related_session_name,\
                 related_provider,related_device_name,is_read,is_resolved"
                    .to_string(),
            ),
            ("order", "created_at.desc".to_string()),
            ("limit", LIMIT.to_string()),
        ])
        .send()
        .await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    if !(200..300).contains(&status) {
        return Err(SupabaseError::Http { status, body: text });
    }
    if text.is_empty() {
        return Ok(vec![]);
    }
    Ok(serde_json::from_str(&text)?)
}

// ========================================================================
// delete_user_account (v0.5.4) — server-side account deletion. RPC takes
// no args; server reads `auth.uid()` from the user JWT and cascades the
// delete through `auth.users`, removing rows in `sessions`,
// `daily_usage_metrics`, `alerts`, `desktop_helpers`, etc. via FK
// cascade. Returns `{success: true}` on completion.
//
// CALLER ORDERING IS LOAD-BEARING (see lib.rs::delete_account_and_unpair):
// the caller MUST mint the user JWT BEFORE clearing keychain — `with_user_jwt`
// reads the refresh_token from keychain to refresh, so a clear-keychain-first
// ordering would ship an unauthenticated request and leave the user thinking
// "account deleted" while the server still has all their rows. Codex P1 +
// Gemini 3.1 Pro P1 (both flagged independently). Local clear runs ONLY
// after the RPC returns Ok; on RPC error, local state is preserved so the
// user can retry without re-pairing.
// ========================================================================

pub async fn delete_user_account(user_jwt: &str) -> SupabaseResult<()> {
    let v = rpc_with_auth(
        "delete_user_account",
        &serde_json::json!({}),
        Some(user_jwt),
    )
    .await?;
    check_rpc_error(&v)?;
    // Server returns `{success: true}` on completion; we discard the body
    // — the only signal we care about is the absence of an RPC error.
    Ok(())
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

    // v0.5.4 — delete_user_account RPC wrapper. We cannot call the live
    // RPC from a unit test (it would actually delete an account); instead
    // we pin the response-shape contract: a `{success: true}` payload
    // round-trips through `check_rpc_error` cleanly, and a
    // `{error: ..., message: ...}` payload becomes `SupabaseError::Rpc`.
    // The same `check_rpc_error` path is shared with every other RPC,
    // so this is also a regression catch for the universal path.

    #[test]
    fn delete_user_account_response_shape_passes_check() {
        let success = json!({"success": true});
        assert!(check_rpc_error(&success).is_ok());
    }

    #[test]
    fn delete_user_account_error_shape_becomes_rpc_error() {
        let err_body = json!({"error": "permission_denied", "message": "JWT missing or invalid"});
        let result = check_rpc_error(&err_body);
        match result {
            Err(SupabaseError::Rpc { code, .. }) => {
                assert_eq!(code, "permission_denied");
            }
            _ => panic!("expected SupabaseError::Rpc"),
        }
    }
}
