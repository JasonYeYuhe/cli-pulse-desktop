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
// sessions history (v0.5.5) — direct PostgREST GET for the Activity
// Timeline chart on the Sessions tab.
// ========================================================================
//
// The v1 plan got the data source wrong: `list_sessions` (the Tauri
// command at lib.rs:115) is a current-process snapshot of THIS device,
// truncated to 12 most-active processes (sessions.rs:311). It is NOT a
// 24h history. The Codex pre-implementation review caught this — the
// Activity Timeline needs the cross-device historical view, which lives
// only in the `sessions` table.
//
// `SessionRow` (used by the v0.5.2 TopProjects card) selects only the
// fields needed for cost aggregation: project, estimated_cost,
// requests, last_active_at, total_usage. The chart needs ALSO:
// `started_at` (for the bar's left edge), `provider` (for the lane),
// and `id` (for the React memo key — using id+last_active_at avoids
// the v1 plan's leaky `sessions.length + sessions[0]?.last_active_at`
// memo key that misses non-first session updates, per Gemini P2).
//
// This is a separate struct rather than extending `SessionRow` because
// the TopProjects path already ships and doesn't need these extra
// fields — keeping the wire shapes minimal helps the 5 000-row
// PostgREST limit cover more sessions.

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SessionHistoryRow {
    pub id: String,
    pub provider: String,
    pub project: Option<String>,
    pub started_at: String,
    pub last_active_at: String,
    #[serde(default)]
    pub estimated_cost: Option<f64>,
    #[serde(default)]
    pub total_usage: Option<i64>,
    #[serde(default)]
    pub requests: Option<i64>,
}

/// Fetch session rows for the Activity Timeline. RLS-filtered to this
/// user, server-side filter `last_active_at >= since`. Ordered by
/// `started_at.desc` so the most-recent sessions render on top of
/// older bars when stacking inside a provider lane (Z-order = recency
/// — usually the most useful for a quick scan).
///
/// LIMIT 1000 covers a 24h window for any realistic user — the
/// pathological case is one session per minute = 1 440 sessions per
/// day, which is well above what any real CLI workflow produces. If
/// the limit kicks in, the chart still renders; it just clips the
/// oldest bars (the user sees `+N more` overflow indicator at the
/// left edge).
pub async fn get_sessions_history(
    user_id: &str,
    since: chrono::DateTime<chrono::Utc>,
    user_jwt: &str,
) -> SupabaseResult<Vec<SessionHistoryRow>> {
    const LIMIT: u32 = 1000;
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
                "id,provider,project,started_at,last_active_at,estimated_cost,total_usage,requests"
                    .to_string(),
            ),
            ("order", "started_at.desc".to_string()),
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
// Remote Approvals — HELPER-side wrappers (v0.7.0).
// ========================================================================
//
// These wrap the `remote_helper_*` RPCs (live since 2026-04-29 Phase 1)
// for the Windows-side hook emission binary. They authenticate with
// (device_id, helper_secret) — same auth model the existing
// helper_sync uses, so the desktop's pairing credentials (already
// stored in HelperConfig) are sufficient.
//
// The hook runs as a Claude-Code subprocess (NOT inside the Tauri
// app), so these functions must be `pub` and not require any Tauri
// runtime state. They re-use the module-level `client()` factory.

/// Wraps `remote_helper_create_permission_request`. Creates a row in
/// `remote_permission_requests` with status='pending'. Server-side
/// gates on `user_settings.remote_control_enabled = true` AND
/// validates `(device_id, helper_secret)`.
///
/// `payload` is JSON-encoded by the caller (typed-only on the
/// adapter level — Rust client stays opaque to keep the wire shape
/// pluggable for future provider adapters).
///
/// `ttl_seconds` is the deadline after which the row auto-expires.
/// The Mac sibling uses 60 s (matches Claude's hook timeout window
/// of 10 s with margin for the polling loop's cleanup).
#[allow(clippy::too_many_arguments)]
pub async fn remote_helper_create_permission_request(
    device_id: &str,
    helper_secret: &str,
    request_id: &str,
    session_id: Option<&str>,
    provider: &str,
    tool_name: &str,
    summary: &str,
    payload: serde_json::Value,
    risk: &str,
    ttl_seconds: u32,
) -> SupabaseResult<()> {
    #[derive(Serialize)]
    struct Params<'a> {
        p_device_id: &'a str,
        p_helper_secret: &'a str,
        p_request_id: &'a str,
        p_session_id: Option<&'a str>,
        p_provider: &'a str,
        p_tool_name: &'a str,
        p_summary: &'a str,
        p_payload: serde_json::Value,
        p_risk: &'a str,
        p_ttl_seconds: u32,
    }
    let v = rpc(
        "remote_helper_create_permission_request",
        &Params {
            p_device_id: device_id,
            p_helper_secret: helper_secret,
            p_request_id: request_id,
            p_session_id: session_id,
            p_provider: provider,
            p_tool_name: tool_name,
            p_summary: summary,
            p_payload: payload,
            p_risk: risk,
            p_ttl_seconds: ttl_seconds,
        },
    )
    .await?;
    check_rpc_error(&v)?;
    Ok(())
}

/// One-shot poll for a decision on `request_id`. Returns:
///   * `Some(("approve" | "deny", scope, reason))` if the user has
///     decided.
///   * `None` if the row is still pending.
///   * `Err` on transport / auth failure (caller falls through to
///     local-prompt fallback).
///
/// The Mac sibling polls every 1 s with a 10 s budget. Stub the same
/// cadence in the hook binary.
#[derive(Debug, Deserialize)]
pub struct HelperPermissionDecision {
    pub status: String, // "pending" / "approved" / "denied" / "expired"
    #[serde(default)]
    pub decision: Option<String>, // "approve" / "deny" — when status != pending
    #[serde(default)]
    pub scope: Option<String>, // "once" / "alwaysSession"
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn remote_helper_poll_permission_decision(
    device_id: &str,
    helper_secret: &str,
    request_id: &str,
) -> SupabaseResult<HelperPermissionDecision> {
    #[derive(Serialize)]
    struct Params<'a> {
        p_device_id: &'a str,
        p_helper_secret: &'a str,
        p_request_id: &'a str,
    }
    let v = rpc(
        "remote_helper_poll_permission_decision",
        &Params {
            p_device_id: device_id,
            p_helper_secret: helper_secret,
            p_request_id: request_id,
        },
    )
    .await?;
    check_rpc_error(&v)?;
    Ok(serde_json::from_value(v)?)
}

// ========================================================================
// Remote Approvals + Sessions (v0.6.0) — app-side wrappers around the
// existing live remote_app_* RPCs.
// ========================================================================
//
// The macOS team (cli-pulse repo) shipped Phase 1 (Remote Approvals,
// v1.11.0/44 on 2026-04-29) and Phase 2 iter1 (Sessions Input,
// 2026-05-03). The backend is fully live in Supabase as of those
// dates: 5 remote_* tables, 6 app RPCs, server-side gate at
// `user_settings.remote_control_enabled` (default OFF).
//
// v0.6.0 ports the APP-SIDE READ + DECIDE surface to Windows. Hook
// emission, ConPTY managed-session host, and Send / Stop / Interrupt
// commands ship in later slices (v0.6.1, v0.7.0, v0.8.0 — see the
// dev plan).
//
// Wire shapes mirror Swift `Models.swift:867-1005` exactly. Optional
// fields use `#[serde(default)]` so server-side schema drift (new
// fields appearing) doesn't break decode for older desktop clients.

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemotePermissionRequest {
    pub id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
    pub provider: String,
    pub tool_name: String,
    pub summary: String,
    /// "low" / "medium" / "high". Frontend renders unknown values as
    /// neutral (Gemini 3.1 Pro v0.6.0 review P2) — server may emit
    /// new risk classes in future versions.
    pub risk: String,
    /// "pending" / "approved" / "denied" / "expired". Same drift
    /// posture as `risk`.
    pub status: String,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteSession {
    pub id: String,
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
    pub provider: String,
    pub cwd_basename: String,
    #[serde(default)]
    pub cwd_hmac: Option<String>,
    /// "pending" / "running" / "stopped" / "errored"
    pub status: String,
    #[serde(default)]
    pub client_label: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub last_event_at: Option<String>,
}

/// Wraps `remote_app_list_pending_approvals()` — RLS-scoped to the
/// authenticated user, so this surfaces approvals from EVERY paired
/// device the user owns (Mac, future Windows-helper, etc). Server
/// gates on `user_settings.remote_control_enabled = true`; returns
/// `[]` when the toggle is off.
pub async fn remote_list_pending_approvals(
    user_jwt: &str,
) -> SupabaseResult<Vec<RemotePermissionRequest>> {
    let v = rpc_with_auth(
        "remote_app_list_pending_approvals",
        &serde_json::json!({}),
        Some(user_jwt),
    )
    .await?;
    check_rpc_error(&v)?;
    // RPC returns a JSON array directly.
    Ok(serde_json::from_value(v)?)
}

/// Wraps `remote_app_decide_permission`. `decision` is "approve" or
/// "deny" (validated server-side); `scope` is "once" or "alwaysSession"
/// (silently downgraded to "once" for Codex).
///
/// `decided_by_device_id` lets the server attribute the decision to
/// THIS Windows device in the audit log — Mac sibling does the same
/// via `cfg.device_id`. Optional because the iOS app calls without it
/// (no helper credentials on iOS).
pub async fn remote_decide_permission(
    request_id: &str,
    decision: &str,
    scope: &str,
    decided_by_device_id: Option<&str>,
    user_jwt: &str,
) -> SupabaseResult<()> {
    #[derive(Serialize)]
    struct Params<'a> {
        p_request_id: &'a str,
        p_decision: &'a str,
        p_scope: &'a str,
        p_decided_by_device_id: Option<&'a str>,
    }
    let v = rpc_with_auth(
        "remote_app_decide_permission",
        &Params {
            p_request_id: request_id,
            p_decision: decision,
            p_scope: scope,
            p_decided_by_device_id: decided_by_device_id,
        },
        Some(user_jwt),
    )
    .await?;
    check_rpc_error(&v)?;
    Ok(())
}

/// Wraps `remote_app_send_command(p_session_id, p_kind, p_payload)`.
///
/// `kind` is "prompt" | "stop" | "interrupt" (validated server-side).
/// `payload` is the prompt text for "prompt" (capped at 8192 chars
/// server-side per the v0.26 column constraint); empty for "stop"
/// and "interrupt".
///
/// Server-side: gated on `user_settings.remote_control_enabled = true`
/// AND on `remote_sessions.user_id = auth.uid()` (RLS-equivalent
/// inside the SECURITY DEFINER function), so a user can only command
/// sessions they own. The helper polls `remote_helper_pull_commands`
/// every ~1s and dispatches by kind.
///
/// Returns `()` on success — the RPC writes the row and returns the
/// command_id, but for v0.6.2 the desktop's Send/Stop UX is
/// fire-and-forget (no wait for the helper to ACK). v0.6.x+ may add
/// a sessions-events stream that surfaces command outcomes; for now
/// the user re-checks via the next poll of `remote_app_list_sessions`.
pub async fn remote_send_command(
    session_id: &str,
    kind: &str,
    payload: Option<&str>,
    user_jwt: &str,
) -> SupabaseResult<()> {
    #[derive(Serialize)]
    struct Params<'a> {
        p_session_id: &'a str,
        p_kind: &'a str,
        p_payload: &'a str,
    }
    let v = rpc_with_auth(
        "remote_app_send_command",
        &Params {
            p_session_id: session_id,
            p_kind: kind,
            p_payload: payload.unwrap_or(""),
        },
        Some(user_jwt),
    )
    .await?;
    check_rpc_error(&v)?;
    Ok(())
}

/// v0.9.1a — Wraps `remote_app_request_session_start`. Creates a row
/// in `remote_sessions(status='pending')` + `remote_session_commands
/// (kind='start')` for the agent to pick up. Originally introduced
/// in v0.8.0; reverted in v0.8.1 along with the rest of ConPTY;
/// restored verbatim in v0.9.1a.
///
/// `target_device_id` is the device that should HOST the session
/// (where the spawned `claude.exe` will run). For v0.9.1a the spawn
/// dialog passes THIS Windows machine's own device_id so the local
/// agent picks it up — but the agent's `StubTransport` returns
/// `Internal("not yet implemented")` so the row gets marked errored
/// on the agent's first tick. v0.9.1b swaps the stub for
/// `ConPtyTransport` and the spawn actually succeeds.
///
/// Returns the new session_id (UUID).
pub async fn remote_request_session_start(
    target_device_id: &str,
    provider: &str,
    cwd_basename: Option<&str>,
    cwd_hmac: Option<&str>,
    client_label: Option<&str>,
    user_jwt: &str,
) -> SupabaseResult<String> {
    #[derive(Serialize)]
    struct Params<'a> {
        p_device_id: &'a str,
        p_provider: &'a str,
        p_cwd_basename: Option<&'a str>,
        p_cwd_hmac: Option<&'a str>,
        p_client_label: Option<&'a str>,
    }
    let v = rpc_with_auth(
        "remote_app_request_session_start",
        &Params {
            p_device_id: target_device_id,
            p_provider: provider,
            p_cwd_basename: cwd_basename,
            p_cwd_hmac: cwd_hmac,
            p_client_label: client_label,
        },
        Some(user_jwt),
    )
    .await?;
    check_rpc_error(&v)?;
    if let Some(id) = v.get("session_id").and_then(|x| x.as_str()) {
        return Ok(id.to_string());
    }
    if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
        return Ok(id.to_string());
    }
    Err(SupabaseError::Json(serde_json::Error::io(
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "remote_app_request_session_start: response missing session_id/id",
        ),
    )))
}

/// Wraps `remote_app_list_sessions()` — Phase 2 iter1 RPC. Returns
/// pending+running managed sessions across the user's devices, with
/// `device_name` joined for display. Returns `[]` when Remote Control
/// is disabled.
pub async fn remote_list_sessions(user_jwt: &str) -> SupabaseResult<Vec<RemoteSession>> {
    let v = rpc_with_auth(
        "remote_app_list_sessions",
        &serde_json::json!({}),
        Some(user_jwt),
    )
    .await?;
    check_rpc_error(&v)?;
    Ok(serde_json::from_value(v)?)
}

// ========================================================================
// Swarm View (v0.10.1 — macOS/iOS parity)
// ========================================================================

/// One swarm (a git repo+branch grouping of sibling agents) within a
/// device heartbeat. Mirrors the macOS `RemoteSwarm` (Models.swift) and
/// the per-element shape stored in `remote_swarms.swarms` (backend
/// migrate_v0.48). `handle` is the opaque `swarm-<6hex>` and `swarm_key`
/// is an account-scoped HMAC — NO repo path or branch name crosses the
/// wire. P0 carries no `$`/token figure; the headline is agents/blocked.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteSwarm {
    pub swarm_key: String,
    pub handle: String,
    #[serde(default)]
    pub is_linked_worktree: bool,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub agents: i64,
    #[serde(default)]
    pub blocked: i64,
    #[serde(default)]
    pub oldest_blocked_age_s: f64,
    #[serde(default)]
    pub last_seen_s_ago: f64,
}

/// One device's swarm heartbeat. Mirrors the macOS `RemoteSwarmDevice`
/// and the object shape returned by `remote_app_list_swarms()`. `stale`
/// is set server-side when the device is past the 90s live-TTL (the UI
/// greys it and shows "last seen" rather than dropping it).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteSwarmDevice {
    pub device_id: String,
    pub updated_at: String,
    #[serde(default)]
    pub age_s: f64,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub swarms: Vec<RemoteSwarm>,
}

/// Wraps `remote_app_list_swarms()` — JWT-authenticated, RLS-scoped to
/// the user, and remote-control-gated server-side (returns `[]` when
/// the user's `remote_control_enabled` is false). Surfaces swarms from
/// EVERY paired device the user owns. Mirrors the macOS
/// `APIClient.remoteListSwarms()`.
pub async fn remote_list_swarms(user_jwt: &str) -> SupabaseResult<Vec<RemoteSwarmDevice>> {
    let v = rpc_with_auth(
        "remote_app_list_swarms",
        &serde_json::json!({}),
        Some(user_jwt),
    )
    .await?;
    check_rpc_error(&v)?;
    // RPC returns a JSON array directly (empty when RC is off).
    Ok(serde_json::from_value(v)?)
}

/// Read `user_settings.remote_control_enabled` for the authenticated
/// user. Direct PostgREST GET — same pattern as v0.5.2 sessions read
/// and v0.5.3 alerts read. Returns `false` when no row exists yet
/// (brand-new account never opened Settings → Privacy on any device).
pub async fn get_remote_control_setting(user_id: &str, user_jwt: &str) -> SupabaseResult<bool> {
    let url = format!("{}/rest/v1/user_settings", creds::supabase_url());
    let anon = creds::supabase_anon_key();
    let resp = client()?
        .get(&url)
        .header("apikey", &anon)
        .header("Authorization", format!("Bearer {user_jwt}"))
        .query(&[
            ("user_id", format!("eq.{user_id}")),
            ("select", "remote_control_enabled".to_string()),
            ("limit", "1".to_string()),
        ])
        .send()
        .await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    if !(200..300).contains(&status) {
        return Err(SupabaseError::Http { status, body: text });
    }
    if text.is_empty() {
        return Ok(false);
    }
    #[derive(Deserialize)]
    struct Row {
        #[serde(default)]
        remote_control_enabled: Option<bool>,
    }
    let rows: Vec<Row> = serde_json::from_str(&text)?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|r| r.remote_control_enabled)
        .unwrap_or(false))
}

/// Write `user_settings.remote_control_enabled` for the authenticated
/// user. UPSERT via POST with `Prefer: resolution=merge-duplicates` —
/// covers the brand-new-user case where `user_settings` has no row
/// yet. The Mac sibling uses PATCH only, but PostgREST PATCH on a
/// non-existent row returns HTTP 2xx with 0 rows affected — the
/// frontend would interpret that as success while the server-side
/// state stays `false`, then the next poll would silently flip the
/// toggle back to OFF (Gemini 3.1 Pro v0.6.0 post-implementation
/// review P1). The Mac may have the same latent bug; UPSERT here
/// fixes it for the desktop track regardless.
///
/// `user_settings.user_id` is the table's PRIMARY KEY (verified via
/// information_schema.constraint definitions); merge-duplicates uses
/// it as the conflict target.
///
/// Per Gemini 3.1 Pro v0.6.0 review P0: caller MUST revert any
/// optimistic UI on this function returning Err — showing "ON" while
/// the server holds "OFF" is a privacy violation.
pub async fn set_remote_control_setting(
    user_id: &str,
    enabled: bool,
    user_jwt: &str,
) -> SupabaseResult<()> {
    let url = format!("{}/rest/v1/user_settings", creds::supabase_url());
    let anon = creds::supabase_anon_key();
    let resp = client()?
        .post(&url)
        .header("apikey", &anon)
        .header("Authorization", format!("Bearer {user_jwt}"))
        .header("Content-Type", "application/json")
        // resolution=merge-duplicates → UPSERT against the primary
        // key (user_id). return=minimal saves bandwidth.
        .header("Prefer", "resolution=merge-duplicates,return=minimal")
        .json(&serde_json::json!({
            "user_id": user_id,
            "remote_control_enabled": enabled,
        }))
        .send()
        .await?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await?;
        return Err(SupabaseError::Http { status, body: text });
    }
    Ok(())
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

// ========================================================================
// v0.9.1a — Remote Sessions HELPER-side wrappers (managed-session host).
// ========================================================================
//
// These four RPCs are already-live (Mac sibling shipped them in
// `cli-pulse` Phase 2 iter1, 2026-05-03). v0.9.1a wires the desktop
// agent loop into the same surfaces so a Windows / Linux host can
// register, dispatch commands, and report events for a managed
// Claude session that another device's UI is driving. v0.9.1a uses
// `StubTransport` so `register_session` is never actually called
// (spawn always fails) — but `pull_commands`, `post_event`, and
// `complete_command` are exercised on every agent tick.
//
// Auth: same `(device_id, helper_secret)` pair the existing
// `remote_helper_create_permission_request` uses (v0.7.0 hook
// emission). HelperConfig already has both — no new credential.
//
// Originally introduced in v0.8.0; reverted in v0.8.1 along with
// the rest of ConPTY; restored in v0.9.1a verbatim.

/// Wraps `remote_helper_register_session`. Called once after the
/// transport spawn succeeds; flips the `remote_sessions.status` row
/// from `pending` → `running` so the originating device's UI shows
/// the session as live.
///
/// Server gates on `(device_id, helper_secret)` matching AND on
/// `user_settings.remote_control_enabled = true`. Off-toggle returns
/// an error variant; caller should still post a `kind=errored` event
/// from the agent so the row doesn't sit at `pending` forever.
#[allow(clippy::too_many_arguments)]
pub async fn remote_helper_register_session(
    device_id: &str,
    helper_secret: &str,
    session_id: &str,
    provider: &str,
    cwd_basename: &str,
    cwd_hmac: Option<&str>,
    client_label: Option<&str>,
) -> SupabaseResult<()> {
    #[derive(Serialize)]
    struct Params<'a> {
        p_device_id: &'a str,
        p_helper_secret: &'a str,
        p_session_id: &'a str,
        p_provider: &'a str,
        p_cwd_basename: &'a str,
        p_cwd_hmac: Option<&'a str>,
        p_client_label: Option<&'a str>,
    }
    let v = rpc(
        "remote_helper_register_session",
        &Params {
            p_device_id: device_id,
            p_helper_secret: helper_secret,
            p_session_id: session_id,
            p_provider: provider,
            p_cwd_basename: cwd_basename,
            p_cwd_hmac: cwd_hmac,
            p_client_label: client_label,
        },
    )
    .await?;
    check_rpc_error(&v)?;
    Ok(())
}

/// One queued command for this device's agent loop. Mirrors the
/// shape `remote_helper_pull_commands` returns. Fields beyond what
/// v0.9.1a dispatches are decoded leniently (Option) so server schema
/// drift doesn't break decode for older desktop clients.
#[derive(Debug, Clone, Deserialize)]
pub struct PulledCommand {
    pub id: String,
    pub session_id: String,
    /// "start" | "prompt" | "stop" | "interrupt". String not enum so
    /// future-class servers don't crash decode.
    pub kind: String,
    /// Free-text payload (prompt body for `kind="prompt"`, JSON
    /// metadata for `kind="start"`, empty for `stop` / `interrupt`).
    #[serde(default)]
    pub payload: Option<String>,
    /// Optional `created_at` timestamp; informational only.
    #[serde(default)]
    pub created_at: Option<String>,
}

/// Wraps `remote_helper_pull_commands`. Returns up to `max` queued
/// commands across all sessions on this device. Server-side gate
/// returns an error when Remote Control is disabled — caller
/// (`agent.rs`) catches and continues; on the next cycle, if the
/// user re-enables, dispatching resumes.
pub async fn remote_helper_pull_commands(
    device_id: &str,
    helper_secret: &str,
    max: u32,
) -> SupabaseResult<Vec<PulledCommand>> {
    #[derive(Serialize)]
    struct Params<'a> {
        p_device_id: &'a str,
        p_helper_secret: &'a str,
        p_max: u32,
    }
    let v = rpc(
        "remote_helper_pull_commands",
        &Params {
            p_device_id: device_id,
            p_helper_secret: helper_secret,
            p_max: max,
        },
    )
    .await?;
    if let Some(obj) = v.as_object() {
        if obj.contains_key("error") {
            check_rpc_error(&v)?;
        }
    }
    if v.is_null() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_value(v)?)
}

/// Wraps `remote_helper_post_event`. Server-side gate transitions
/// `remote_sessions.status` only when `p_kind = 'status'` AND
/// `p_payload IN ('stopped', 'errored')`. See `events.rs` for the
/// defensive enum that prevents typos.
pub async fn remote_helper_post_event(
    device_id: &str,
    helper_secret: &str,
    session_id: &str,
    seq: i64,
    kind: &str,
    payload: &str,
) -> SupabaseResult<()> {
    #[derive(Serialize)]
    struct Params<'a> {
        p_device_id: &'a str,
        p_helper_secret: &'a str,
        p_session_id: &'a str,
        p_seq: i64,
        p_kind: &'a str,
        p_payload: &'a str,
    }
    let v = rpc(
        "remote_helper_post_event",
        &Params {
            p_device_id: device_id,
            p_helper_secret: helper_secret,
            p_session_id: session_id,
            p_seq: seq,
            p_kind: kind,
            p_payload: payload,
        },
    )
    .await?;
    check_rpc_error(&v)?;
    Ok(())
}

/// Wraps `remote_helper_complete_command`. `status` is `"delivered"`
/// or `"failed"`. `error` is a short reason string; passed when
/// status == "failed". Mac sibling stores at most 200 chars in this
/// column; we don't enforce that client-side (server CHECK does).
pub async fn remote_helper_complete_command(
    device_id: &str,
    helper_secret: &str,
    command_id: &str,
    status: &str,
    error: Option<&str>,
) -> SupabaseResult<()> {
    #[derive(Serialize)]
    struct Params<'a> {
        p_device_id: &'a str,
        p_helper_secret: &'a str,
        p_command_id: &'a str,
        p_status: &'a str,
        p_error: Option<&'a str>,
    }
    let v = rpc(
        "remote_helper_complete_command",
        &Params {
            p_device_id: device_id,
            p_helper_secret: helper_secret,
            p_command_id: command_id,
            p_status: status,
            p_error: error,
        },
    )
    .await?;
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

    // v0.5.5 — SessionHistoryRow deserialization. The PostgREST GET
    // response is a JSON array of session rows; we pin the parse
    // contract for the timeline-specific fields. Drift in the
    // server-side `sessions` table column types (e.g. `started_at`
    // becoming a non-string ISO format) would silently break the
    // chart rendering — this test catches it at unit-time, not VM-
    // verify-time.

    #[test]
    fn session_history_row_round_trips() {
        let body = json!([{
            "id": "proc-12345",
            "provider": "claude",
            "project": "my-project",
            "started_at": "2026-05-06T00:00:00+00:00",
            "last_active_at": "2026-05-06T00:30:00+00:00",
            "estimated_cost": 0.42,
            "total_usage": 12345,
            "requests": 7
        }]);
        let rows: Vec<SessionHistoryRow> = serde_json::from_value(body).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "proc-12345");
        assert_eq!(rows[0].provider, "claude");
        assert_eq!(rows[0].project.as_deref(), Some("my-project"));
        assert_eq!(rows[0].estimated_cost, Some(0.42));
    }

    // v0.6.0 — Remote Approvals wire-shape pins. Mirrors Swift
    // Models.swift:867-1005. These tests catch server-side schema drift
    // (new fields appearing OR existing fields' types changing) at unit
    // time, before VM verify.

    #[test]
    fn remote_permission_request_round_trips_full_shape() {
        let body = json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "session_id": "22222222-2222-2222-2222-222222222222",
            "device_id": "33333333-3333-3333-3333-333333333333",
            "device_name": "Jason's MBP",
            "provider": "claude",
            "tool_name": "Bash",
            "summary": "rm test/fixture.json",
            "risk": "low",
            "status": "pending",
            "created_at": "2026-05-06T12:00:00+00:00",
            "expires_at": "2026-05-06T12:00:10+00:00"
        });
        let req: RemotePermissionRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(req.device_name.as_deref(), Some("Jason's MBP"));
        assert_eq!(req.risk, "low");
    }

    #[test]
    fn remote_permission_request_handles_missing_optional_fields() {
        // device_name was added in v0.32 (Mac iter); session_id is
        // optional for hand-Terminal flows. Older / leaner server
        // responses must still decode cleanly.
        let body = json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "device_id": "33333333-3333-3333-3333-333333333333",
            "provider": "claude",
            "tool_name": "Bash",
            "summary": "...",
            "risk": "high",
            "status": "pending",
            "created_at": "2026-05-06T12:00:00+00:00",
            "expires_at": "2026-05-06T12:00:10+00:00"
        });
        let req: RemotePermissionRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.session_id, None);
        assert_eq!(req.device_name, None);
    }

    #[test]
    fn remote_permission_request_handles_unknown_risk_class() {
        // Per Gemini 3.1 Pro v0.6.0 review P2: server may emit risk
        // values beyond low/medium/high in future versions; we use
        // String not enum so they decode without error and the frontend
        // renders an unknown class as neutral.
        let body = json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "device_id": "33333333-3333-3333-3333-333333333333",
            "provider": "claude",
            "tool_name": "Bash",
            "summary": "...",
            "risk": "future-class-not-yet-existing",
            "status": "pending",
            "created_at": "2026-05-06T12:00:00+00:00",
            "expires_at": "2026-05-06T12:00:10+00:00"
        });
        let req: RemotePermissionRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.risk, "future-class-not-yet-existing");
    }

    #[test]
    fn remote_session_round_trips_full_shape() {
        let body = json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "device_id": "33333333-3333-3333-3333-333333333333",
            "device_name": "Jason's MBP",
            "provider": "claude",
            "cwd_basename": "my-project",
            "cwd_hmac": "deadbeef",
            "status": "running",
            "client_label": "Mac · my-project",
            "created_at": "2026-05-06T12:00:00+00:00",
            "last_event_at": "2026-05-06T12:05:00+00:00"
        });
        let s: RemoteSession = serde_json::from_value(body).unwrap();
        assert_eq!(s.device_name.as_deref(), Some("Jason's MBP"));
        assert_eq!(s.status, "running");
        assert_eq!(
            s.last_event_at.as_deref(),
            Some("2026-05-06T12:05:00+00:00")
        );
    }

    #[test]
    fn remote_session_handles_missing_optional_fields() {
        let body = json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "device_id": "33333333-3333-3333-3333-333333333333",
            "provider": "claude",
            "cwd_basename": "my-project",
            "status": "pending",
            "created_at": "2026-05-06T12:00:00+00:00"
        });
        let s: RemoteSession = serde_json::from_value(body).unwrap();
        assert_eq!(s.device_name, None);
        assert_eq!(s.cwd_hmac, None);
        assert_eq!(s.client_label, None);
        assert_eq!(s.last_event_at, None);
    }

    #[test]
    fn remote_swarm_list_round_trips_full_shape() {
        // Mirrors the `remote_app_list_swarms()` output (migrate_v0.48):
        // an array of device objects, each with a nested `swarms` array.
        let body = json!([{
            "device_id": "33333333-3333-3333-3333-333333333333",
            "updated_at": "2026-06-21T00:00:00+00:00",
            "age_s": 12.3,
            "stale": false,
            "swarms": [{
                "swarm_key": "abc123hmac",
                "handle": "swarm-1a2b3c",
                "is_linked_worktree": true,
                "providers": ["Claude", "Codex"],
                "agents": 4,
                "blocked": 1,
                "oldest_blocked_age_s": 95.0,
                "last_seen_s_ago": 8.0
            }]
        }]);
        let devices: Vec<RemoteSwarmDevice> = serde_json::from_value(body).unwrap();
        assert_eq!(devices.len(), 1);
        assert!(!devices[0].stale);
        assert_eq!(devices[0].swarms.len(), 1);
        let s = &devices[0].swarms[0];
        assert_eq!(s.handle, "swarm-1a2b3c");
        assert_eq!(s.agents, 4);
        assert_eq!(s.blocked, 1);
        assert!(s.is_linked_worktree);
        assert_eq!(s.providers, vec!["Claude", "Codex"]);
    }

    #[test]
    fn remote_swarm_handles_empty_and_missing_optionals() {
        // RC-off / no devices → empty array decodes to empty Vec.
        let empty: Vec<RemoteSwarmDevice> = serde_json::from_value(json!([])).unwrap();
        assert!(empty.is_empty());
        // A device with no swarms and a swarm missing optional fields
        // (only the two required keys) must still decode via serde
        // defaults — matches the "drift posture" of the other wire types.
        let body = json!([{
            "device_id": "d1",
            "updated_at": "2026-06-21T00:00:00+00:00",
            "swarms": [{ "swarm_key": "k", "handle": "swarm-000000" }]
        }]);
        let devices: Vec<RemoteSwarmDevice> = serde_json::from_value(body).unwrap();
        assert_eq!(devices[0].age_s, 0.0);
        assert!(!devices[0].stale);
        let s = &devices[0].swarms[0];
        assert_eq!(s.agents, 0);
        assert_eq!(s.blocked, 0);
        assert!(s.providers.is_empty());
        assert!(!s.is_linked_worktree);
    }

    // v0.8.0 introduced PulledCommand wire-shape tests; v0.8.1 removes
    // them along with the type itself (the agent loop that consumed
    // them is gone in this revert).

    #[test]
    fn session_history_row_handles_null_project_and_cost() {
        // Sessions can have null project (helper-launched, root on
        // Linux) and null estimated_cost (cost not yet rolled up by
        // the server). Both must deserialize cleanly — the chart's
        // tooltip handles the null values explicitly. v0.5.2 hit
        // this same edge case for TopProjects; pin it here too so
        // a future column rename doesn't silently break.
        let body = json!([{
            "id": "proc-67890",
            "provider": "openrouter",
            "project": null,
            "started_at": "2026-05-06T00:00:00+00:00",
            "last_active_at": "2026-05-06T00:05:00+00:00",
            "estimated_cost": null,
            "total_usage": null,
            "requests": null
        }]);
        let rows: Vec<SessionHistoryRow> = serde_json::from_value(body).unwrap();
        assert_eq!(rows[0].project, None);
        assert_eq!(rows[0].estimated_cost, None);
        assert_eq!(rows[0].total_usage, None);
        assert_eq!(rows[0].requests, None);
    }
}
