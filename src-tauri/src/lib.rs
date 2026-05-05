//! CLI Pulse Desktop — Tauri backend entry point.
//!
//! Sprint 0: local JSONL scan + per-day/model/provider aggregation.
//! Sprint 1: Supabase pairing, config persistence, helper_sync
//! round-trips, periodic 2-minute sync tick.

pub mod alerts;
pub mod auth;
pub mod cache;
pub mod config;
pub mod creds;
pub mod keychain;
pub mod notify;
pub mod paths;
pub mod pricing;
pub mod provider_creds;
pub mod quota;
pub mod scanner;
pub mod sentry_init;
pub mod sessions;
pub mod supabase;
pub mod tray;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use config::HelperConfig;
use once_cell::sync::Lazy;
use scanner::ScanResult;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::async_runtime;
use tokio::sync::mpsc;

const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEVICE_TYPE_WIN: &str = "Windows";
const DEVICE_TYPE_LINUX: &str = "Linux";
const DEVICE_TYPE_MAC: &str = "macOS";
const SYNC_INTERVAL: Duration = Duration::from_secs(120);

fn device_type() -> &'static str {
    if cfg!(target_os = "windows") {
        DEVICE_TYPE_WIN
    } else if cfg!(target_os = "linux") {
        DEVICE_TYPE_LINUX
    } else if cfg!(target_os = "macos") {
        DEVICE_TYPE_MAC
    } else {
        "Desktop"
    }
}

fn system_label() -> String {
    let host = hostname().unwrap_or_else(|| "desktop".to_string());
    format!("{} ({})", host, device_type())
}

fn hostname() -> Option<String> {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    if let Ok(h) = std::env::var("COMPUTERNAME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    None
}

// ------------------------------------------------------------------------
// Commands — exposed to the React frontend via invoke().
// ------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ConfigView {
    paired: bool,
    device_id: Option<String>,
    device_name: Option<String>,
    device_type: String,
    helper_version: String,
    user_id: Option<String>,
}

impl ConfigView {
    fn from_optional(cfg: Option<&HelperConfig>) -> Self {
        Self {
            paired: cfg.is_some(),
            device_id: cfg.map(|c| c.device_id.clone()),
            device_name: cfg.map(|c| c.device_name.clone()),
            device_type: device_type().to_string(),
            helper_version: HELPER_VERSION.to_string(),
            user_id: cfg.map(|c| c.user_id.clone()),
        }
    }
}

#[tauri::command]
fn get_config() -> Result<ConfigView, String> {
    let cfg = config::load().map_err(|e| e.to_string())?;
    Ok(ConfigView::from_optional(cfg.as_ref()))
}

#[tauri::command]
fn scan_usage(days: Option<u32>) -> Result<ScanResult, String> {
    let days = days.unwrap_or(30).clamp(1, 180);
    scanner::scan(days).map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_sessions() -> Result<sessions::SessionsSnapshot, String> {
    async_runtime::spawn_blocking(sessions::collect_sessions)
        .await
        .map_err(|e| format!("sessions join error: {e}"))
}

/// Return the user's current alert thresholds (budget etc.). Never fails —
/// falls back to defaults if no config exists yet.
#[tauri::command]
fn get_thresholds() -> Result<alerts::AlertThresholds, String> {
    let cfg = config::load().map_err(|e| e.to_string())?;
    Ok(cfg.map(|c| c.thresholds).unwrap_or_default())
}

/// Persist budget + CPU spike thresholds. Requires the device to be
/// paired — the thresholds live inside `HelperConfig`, and that file
/// is only created during `pair_device`.
#[tauri::command]
fn set_thresholds(thresholds: alerts::AlertThresholds) -> Result<(), String> {
    let mut cfg = config::load()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Device not paired — pair first, then set budgets".to_string())?;
    cfg.thresholds = thresholds;
    config::save(&cfg).map_err(|e| e.to_string())
}

#[derive(Debug, Serialize)]
struct DiagnosticSnapshot {
    app_version: String,
    os: String,
    arch: String,
    family: String, // "windows" | "linux" | "macos" — std::env::consts::OS
    paired: bool,
    device_id_short: Option<String>,
    cache_dir: Option<String>,
    /// v0.3.4 — log directory path so bug-report copy-paste includes
    /// where to grab `cli-pulse.log` from. Resolved via Tauri's
    /// `app_log_dir()` which matches the path tauri-plugin-log uses
    /// at runtime.
    log_dir: Option<String>,
    /// v0.4.16 — surface which storage backend `provider_creds`
    /// chose at startup. Lets security-conscious users (esp. on
    /// headless Linux) verify they're on the OS keychain not the
    /// plaintext file. Per Gemini 3.1 Pro review: silent fallback
    /// can mislead users; the diagnostic copy makes it visible.
    provider_creds_backend: provider_creds::Backend,
}

/// Used by the About panel to render a copyable diagnostic block when
/// users report issues. Avoids leaking the full helper_secret or
/// user_id — only the first 8 chars of device_id are exposed.
#[tauri::command]
fn diagnostic_snapshot(app: tauri::AppHandle) -> Result<DiagnosticSnapshot, String> {
    use tauri::Manager;
    let cfg = config::load().map_err(|e| e.to_string())?;
    let cache_dir =
        cache::cache_path("codex", None).and_then(|p| p.parent().map(|d| d.display().to_string()));
    let log_dir = app
        .path()
        .app_log_dir()
        .ok()
        .map(|p| p.display().to_string());
    Ok(DiagnosticSnapshot {
        app_version: HELPER_VERSION.to_string(),
        os: device_type().to_string(),
        arch: std::env::consts::ARCH.to_string(),
        family: std::env::consts::OS.to_string(),
        paired: cfg.is_some(),
        device_id_short: cfg
            .as_ref()
            .map(|c| c.device_id.chars().take(8).collect::<String>()),
        cache_dir,
        log_dir,
        provider_creds_backend: provider_creds::current_backend(),
    })
}

/// v0.4.22 — fire a tagged test event into Sentry for ingestion
/// verification. Lets the user (or the VM verifier) confirm the
/// instrumentation chain (init → DSN → network → org-side intake) is
/// actually live, not just "no events because nothing has panicked."
///
/// The lifetime issue count for the `desktop` Sentry project has been
/// 0 since instrumentation went in (per `reference_sentry.md`). That
/// could mean either the app is panic-free OR the DSN never reached
/// the server. Without a deliberate emit path, the two are
/// indistinguishable. v0.4.22 collapses that ambiguity.
///
/// Returns `Ok` on success regardless of whether Sentry is configured
/// — `capture_message` on a no-DSN client is a documented no-op. The
/// caller's UI shows a "sent — check the Sentry dashboard" toast and
/// the user verifies receipt out-of-band. If DSN is missing the
/// toast still shows but Sentry never receives anything; per the
/// `before_send` filter, no PII can leak from this fixed-string
/// path even in worst case.
#[tauri::command]
fn emit_test_sentry_event() -> Result<(), String> {
    sentry::with_scope(
        |scope| {
            // Tag the event so Jason can filter to it on the dashboard
            // (`is:diagnostic-test`) without it polluting real-incident
            // queries. `release` and `platform` tags are already set
            // globally in `sentry_init::install`.
            scope.set_tag("diagnostic_test", "true");
            scope.set_tag(
                "emitted_at",
                chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            );
        },
        || {
            sentry::capture_message(
                "CLI Pulse desktop diagnostic test event — manual emit, ignore.",
                sentry::Level::Info,
            );
        },
    );
    log::info!(
        "emit_test_sentry_event — fired diagnostic test event (Sentry no-op if no DSN configured)"
    );
    Ok(())
}

/// Compute client-side alerts from the current scan + sessions snapshot.
/// Frontend uses this to populate the Alerts tab without waiting for the
/// 2-minute background sync.
#[tauri::command]
async fn preview_alerts() -> Result<Vec<alerts::Alert>, String> {
    let cfg = config::load().map_err(|e| e.to_string())?;
    let (thresholds, device_name) = match cfg.as_ref() {
        Some(c) => (c.thresholds.clone(), Some(c.device_name.clone())),
        None => (alerts::AlertThresholds::default(), None),
    };
    let scan = async_runtime::spawn_blocking(|| scanner::scan(30))
        .await
        .map_err(|e| format!("scanner join error: {e}"))?
        .map_err(|e| e.to_string())?;
    let snapshot = async_runtime::spawn_blocking(sessions::collect_sessions)
        .await
        .map_err(|e| format!("sessions join error: {e}"))?;
    Ok(alerts::compute(
        &scan,
        &snapshot,
        &thresholds,
        device_name.as_deref(),
    ))
}

#[derive(Debug, Serialize)]
struct PairResult {
    device_id: String,
    user_id: String,
    device_name: String,
}

#[tauri::command]
async fn pair_device(
    app: tauri::AppHandle,
    pairing_code: String,
    device_name: Option<String>,
) -> Result<PairResult, String> {
    let code = pairing_code.trim();
    if code.is_empty() {
        return Err("Pairing code is empty".into());
    }
    let name = device_name.unwrap_or_default().trim().to_string();
    let name = if name.is_empty() {
        system_label()
    } else {
        name
    };

    let req = supabase::RegisterHelperRequest {
        p_pairing_code: code,
        p_device_name: &name,
        p_device_type: device_type(),
        p_system: &system_label(),
        p_helper_version: HELPER_VERSION,
    };
    let resp = supabase::register_helper(&req).await.map_err(friendly)?;
    let cfg = HelperConfig {
        device_id: resp.device_id.clone(),
        user_id: resp.user_id.clone(),
        device_name: name.clone(),
        helper_version: HELPER_VERSION.to_string(),
        helper_secret: resp.helper_secret,
        thresholds: alerts::AlertThresholds::default(),
        email: String::new(),
    };
    config::save(&cfg).map_err(|e| format!("Failed to save config: {e}"))?;
    notify::pair_success(&app, &name);
    Ok(PairResult {
        device_id: resp.device_id,
        user_id: resp.user_id,
        device_name: name,
    })
}

/// v0.3.4 — server-side unpair status for the frontend's banner copy.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum UnpairServerStatus {
    /// Server confirmed the row is gone (DELETE succeeded).
    Deleted,
    /// Server says the row was already absent. Idempotent success.
    AlreadyGone,
    /// Network error / 5xx / parse fail. Local clear still ran; the
    /// server may or may not have received the unregister. Frontend
    /// should surface this as a soft caveat.
    Transient { detail: String },
    /// HelperConfig was already empty when unpair was called. Nothing
    /// to do server-side.
    Skipped,
}

#[derive(Debug, Serialize)]
struct UnpairResult {
    server_status: UnpairServerStatus,
}

#[tauri::command]
async fn unpair_device() -> Result<UnpairResult, String> {
    // v0.3.4 — best-effort server unregister before clearing local. Codex
    // review §5.4: classify the server response so the UI can surface a
    // caveat on transient failures (network blips). Local clear runs in
    // ALL branches — trapping the user as "still paired" because their
    // network blipped is worse UX than leaving an orphan server row,
    // which the next sign-in supersedes via register_desktop_helper.
    let server_status = match config::load() {
        Ok(Some(cfg)) => {
            let req = supabase::UnregisterDesktopHelperRequest {
                p_device_id: &cfg.device_id,
                p_helper_secret: &cfg.helper_secret,
            };
            match supabase::unregister_desktop_helper(&req).await {
                Ok(resp) if resp.deleted => {
                    log::info!(
                        "server unregister ok ({} devices remaining)",
                        resp.remaining_devices
                    );
                    UnpairServerStatus::Deleted
                }
                Ok(_) => {
                    log::info!("server row already absent (idempotent)");
                    UnpairServerStatus::AlreadyGone
                }
                Err(e) => {
                    log::warn!("server unregister failed (continuing local clear): {e}");
                    UnpairServerStatus::Transient {
                        detail: friendly(e),
                    }
                }
            }
        }
        _ => UnpairServerStatus::Skipped,
    };

    // Always clear local state.
    let _ = keychain::delete_refresh_token();
    cache_invalidate();
    config::clear().map_err(|e| e.to_string())?;

    Ok(UnpairResult { server_status })
}

// ------------------------------------------------------------------------
// v0.3.0 — direct email OTP sign-in commands.
// ------------------------------------------------------------------------

#[tauri::command]
async fn auth_send_otp(email: String) -> Result<(), String> {
    let email = email.trim();
    if email.is_empty() {
        return Err("Email is empty".into());
    }
    auth::send_otp(email).await.map_err(auth_friendly)
}

#[tauri::command]
async fn auth_verify_otp(
    app: tauri::AppHandle,
    email: String,
    code: String,
    device_name: Option<String>,
) -> Result<PairResult, String> {
    let email = email.trim().to_string();
    let code = code.trim().to_string();
    if email.is_empty() || code.is_empty() {
        return Err("Email or code is empty".into());
    }

    // 1. Verify OTP → tokens.
    let session = auth::verify_otp(&email, &code)
        .await
        .map_err(auth_friendly)?;

    // 2. Mint device credentials with the user JWT.
    let resolved_name = device_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(default_device_name);
    let req = supabase::RegisterDesktopHelperRequest {
        p_device_name: &resolved_name,
        p_device_type: device_type(),
        p_system: &system_label(),
        p_helper_version: HELPER_VERSION,
    };
    let resp = supabase::register_desktop_helper(&req, &session.access_token)
        .await
        .map_err(friendly)?;

    // 3. Persist refresh token to OS keychain. Fail-closed if the
    //    backend is missing — that signal needs to reach the UI so the
    //    user knows to install libsecret (Linux) or fall back to Mac
    //    pairing.
    if let Err(e) = keychain::store_refresh_token(&session.refresh_token) {
        // Wipe whatever we just registered server-side — leaving an
        // orphaned device row that the user can't sign back into is
        // worse UX than asking them to retry.
        let _ = keychain::delete_refresh_token();
        return Err(match e {
            keychain::KeychainError::NotAvailable => {
                "OS keychain not available. On Linux, install libsecret (e.g. `gnome-keyring` \
                 or `kwalletd`); on minimal headless servers, use the Mac pairing flow instead."
                    .to_string()
            }
            keychain::KeychainError::Backend(msg) => format!("Keychain error: {msg}"),
        });
    }

    // 4. Save HelperConfig.
    let cfg = HelperConfig {
        device_id: resp.device_id.clone(),
        user_id: resp.user_id.clone(),
        device_name: resolved_name.clone(),
        helper_version: HELPER_VERSION.to_string(),
        helper_secret: resp.helper_secret,
        thresholds: alerts::AlertThresholds::default(),
        email: session.email.clone(),
    };
    config::save(&cfg).map_err(|e| format!("Failed to save config: {e}"))?;
    // v0.3.4 — clear any prior cache so the new user_id sees fresh data.
    cache_invalidate();
    notify::pair_success(&app, &resolved_name);
    Ok(PairResult {
        device_id: resp.device_id,
        user_id: resp.user_id,
        device_name: resolved_name,
    })
}

#[derive(Debug, Serialize)]
struct AuthStatus {
    paired: bool,
    email: String,
    has_refresh_token: bool,
}

#[tauri::command]
fn auth_status() -> Result<AuthStatus, String> {
    let cfg = config::load().map_err(|e| e.to_string())?;
    let has_refresh_token = matches!(keychain::read_refresh_token(), Ok(Some(_)));
    Ok(AuthStatus {
        paired: cfg.is_some(),
        email: cfg.as_ref().map(|c| c.email.clone()).unwrap_or_default(),
        has_refresh_token,
    })
}

/// Local-only sign-out. Always succeeds regardless of refresh-token
/// state — the goal is "this device is no longer signed in", and that
/// goal is achieved by clearing the local credentials.
#[tauri::command]
fn auth_sign_out() -> Result<(), String> {
    let _ = keychain::delete_refresh_token();
    cache_invalidate();
    config::clear().map_err(|e| e.to_string())
}

/// Probe whether the device + account are still healthy server-side.
/// Used by the helper_sync error classifier (v0.3.0) to distinguish
/// "device removed by another client / account deleted" from a
/// transient 401.
#[tauri::command]
async fn auth_account_check() -> Result<String, String> {
    let cfg = config::load()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Device not paired".to_string())?;
    let req = supabase::DeviceStatusRequest {
        p_device_id: &cfg.device_id,
        p_helper_secret: &cfg.helper_secret,
    };
    let status = supabase::device_status(&req).await.map_err(friendly)?;
    Ok(match status {
        supabase::DeviceStatus::Healthy => "healthy",
        supabase::DeviceStatus::DeviceMissing => "device_missing",
        supabase::DeviceStatus::AccountMissing => "account_missing",
    }
    .to_string())
}

#[tauri::command]
fn auth_default_device_name() -> String {
    default_device_name()
}

fn default_device_name() -> String {
    let dn = whoami::devicename();
    if !dn.trim().is_empty() {
        dn
    } else {
        whoami::fallible::hostname().unwrap_or_else(|_| "Desktop".to_string())
    }
}

fn auth_friendly(e: auth::AuthError) -> String {
    match e {
        auth::AuthError::RateLimited => {
            "Too many tries — please wait a minute and try again.".to_string()
        }
        auth::AuthError::InvalidCode => "Invalid or expired code.".to_string(),
        auth::AuthError::RefreshFailed => "Your sign-in expired. Please sign in again.".to_string(),
        auth::AuthError::Network(err) => format!("Network error: {err}"),
        auth::AuthError::Other { status, body } => format!("Auth error (HTTP {status}): {body}"),
        auth::AuthError::Json(err) => format!("Auth response parse error: {err}"),
    }
}

// ------------------------------------------------------------------------
// v0.3.4 — User-scoped dashboard reads. The desktop pulls server-side
// aggregated state (provider quotas/tiers, cross-device today metrics,
// daily-usage history) so the Providers and Overview tabs reach parity
// with the iOS / Android dashboard.
//
// Auth: each command refreshes the OTP refresh_token on demand to
// obtain a short-lived access_token, then calls the relevant RPC with
// it. The rotated refresh_token is persisted to the keychain BEFORE
// the RPC call (Codex review §5.5 — without this, a process crash
// after refresh but before RPC would lose the new token and lock the
// user out within ~24hrs as Supabase rotates per call).
//
// Cache: in-memory, 30s TTL, anchored by user_id and explicitly
// invalidated at every auth transition (sign-in, sign-out, unpair,
// helper_sync error classifier device_missing/account_missing,
// refresh_token revocation).
// ------------------------------------------------------------------------

const DASHBOARD_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct DashboardCache {
    user_id: String,
    dashboard: Option<(std::time::Instant, supabase::DashboardSummary)>,
    providers: Option<(std::time::Instant, Vec<supabase::ProviderSummaryRow>)>,
    daily_usage: Option<(std::time::Instant, Vec<supabase::DailyUsageRow>)>,
}

static DASHBOARD_CACHE: Lazy<std::sync::Mutex<Option<DashboardCache>>> =
    Lazy::new(|| std::sync::Mutex::new(None));

fn cache_invalidate() {
    if let Ok(mut g) = DASHBOARD_CACHE.lock() {
        *g = None;
    }
}

fn cache_get_dashboard(user_id: &str) -> Option<supabase::DashboardSummary> {
    let g = DASHBOARD_CACHE.lock().ok()?;
    let c = g.as_ref()?;
    if c.user_id != user_id {
        return None;
    }
    let (t, ref v) = *c.dashboard.as_ref()?;
    if t.elapsed() < DASHBOARD_CACHE_TTL {
        Some(v.clone())
    } else {
        None
    }
}

fn cache_put_dashboard(user_id: &str, v: supabase::DashboardSummary) {
    if let Ok(mut g) = DASHBOARD_CACHE.lock() {
        let c = g.get_or_insert_with(|| DashboardCache {
            user_id: user_id.to_string(),
            dashboard: None,
            providers: None,
            daily_usage: None,
        });
        if c.user_id != user_id {
            *c = DashboardCache {
                user_id: user_id.to_string(),
                dashboard: None,
                providers: None,
                daily_usage: None,
            };
        }
        c.dashboard = Some((std::time::Instant::now(), v));
    }
}

fn cache_get_providers(user_id: &str) -> Option<Vec<supabase::ProviderSummaryRow>> {
    let g = DASHBOARD_CACHE.lock().ok()?;
    let c = g.as_ref()?;
    if c.user_id != user_id {
        return None;
    }
    let (t, ref v) = *c.providers.as_ref()?;
    if t.elapsed() < DASHBOARD_CACHE_TTL {
        Some(v.clone())
    } else {
        None
    }
}

fn cache_put_providers(user_id: &str, v: Vec<supabase::ProviderSummaryRow>) {
    if let Ok(mut g) = DASHBOARD_CACHE.lock() {
        let c = g.get_or_insert_with(|| DashboardCache {
            user_id: user_id.to_string(),
            dashboard: None,
            providers: None,
            daily_usage: None,
        });
        if c.user_id != user_id {
            *c = DashboardCache {
                user_id: user_id.to_string(),
                dashboard: None,
                providers: None,
                daily_usage: None,
            };
        }
        c.providers = Some((std::time::Instant::now(), v));
    }
}

fn cache_get_daily_usage(user_id: &str) -> Option<Vec<supabase::DailyUsageRow>> {
    let g = DASHBOARD_CACHE.lock().ok()?;
    let c = g.as_ref()?;
    if c.user_id != user_id {
        return None;
    }
    let (t, ref v) = *c.daily_usage.as_ref()?;
    if t.elapsed() < DASHBOARD_CACHE_TTL {
        Some(v.clone())
    } else {
        None
    }
}

fn cache_put_daily_usage(user_id: &str, v: Vec<supabase::DailyUsageRow>) {
    if let Ok(mut g) = DASHBOARD_CACHE.lock() {
        let c = g.get_or_insert_with(|| DashboardCache {
            user_id: user_id.to_string(),
            dashboard: None,
            providers: None,
            daily_usage: None,
        });
        if c.user_id != user_id {
            *c = DashboardCache {
                user_id: user_id.to_string(),
                dashboard: None,
                providers: None,
                daily_usage: None,
            };
        }
        c.daily_usage = Some((std::time::Instant::now(), v));
    }
}

/// Wrap a user-JWT-scoped RPC call: read refresh_token from keychain,
/// refresh it, persist the rotated token, then call the inner closure
/// with the fresh access_token. Returns a `String` on error so the
/// frontend can render whatever auth_friendly produced.
async fn with_user_jwt<F, Fut, T>(call: F) -> Result<T, String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, supabase::SupabaseError>>,
{
    // 1. Read the persisted refresh_token from the OS keychain.
    let refresh_token = match keychain::read_refresh_token() {
        Ok(Some(t)) => t,
        Ok(None) => return Err("Sign in required to view dashboard data.".into()),
        Err(e) => return Err(format!("Keychain unavailable: {e:?}")),
    };

    // 2. Refresh → fresh AuthSession.
    let session = match auth::refresh(&refresh_token).await {
        Ok(s) => s,
        Err(auth::AuthError::RefreshFailed) => {
            // Refresh token revoked / expired. Clear keychain so the UI
            // re-prompts for sign-in. HelperConfig stays — sync continues
            // working via helper_secret. Clear cache too.
            let _ = keychain::delete_refresh_token();
            cache_invalidate();
            return Err("Session expired — sign in again to view dashboard.".into());
        }
        Err(e) => return Err(auth_friendly(e)),
    };

    // 3. Persist the rotated refresh_token BEFORE the RPC. If we crash
    //    between refresh and RPC, the next boot's refresh sees the new
    //    token instead of the rotated-out old one.
    if let Err(e) = keychain::store_refresh_token(&session.refresh_token) {
        log::error!("failed to persist rotated refresh_token: {e:?}");
        // Continue — this RPC still works; only future refreshes are at
        // risk if persistence fails permanently.
    }

    // 4. Call the RPC.
    call(session.access_token).await.map_err(friendly)
}

#[tauri::command]
async fn get_dashboard_summary() -> Result<supabase::DashboardSummary, String> {
    let cfg = config::load()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Sign in required to view dashboard data.".to_string())?;
    if let Some(cached) = cache_get_dashboard(&cfg.user_id) {
        return Ok(cached);
    }
    let v = with_user_jwt(|jwt| async move { supabase::dashboard_summary(&jwt).await }).await?;
    cache_put_dashboard(&cfg.user_id, v.clone());
    Ok(v)
}

#[tauri::command]
async fn get_provider_summary() -> Result<Vec<supabase::ProviderSummaryRow>, String> {
    let cfg = config::load()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Sign in required to view dashboard data.".to_string())?;
    if let Some(cached) = cache_get_providers(&cfg.user_id) {
        return Ok(cached);
    }
    let v = with_user_jwt(|jwt| async move { supabase::provider_summary(&jwt).await }).await?;
    cache_put_providers(&cfg.user_id, v.clone());
    Ok(v)
}

#[tauri::command]
async fn get_daily_usage(days: Option<u32>) -> Result<Vec<supabase::DailyUsageRow>, String> {
    let days = days.unwrap_or(30).clamp(1, 90);
    let cfg = config::load()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Sign in required to view dashboard data.".to_string())?;
    // Cache scoped per (user, days) — for v0.3.4 we only call with days=30
    // from the UI, so keying on user_id alone is sufficient. If we add
    // multiple windows later, extend the cache key.
    if let Some(cached) = cache_get_daily_usage(&cfg.user_id) {
        return Ok(cached);
    }
    let v = with_user_jwt(|jwt| async move { supabase::get_daily_usage(days, &jwt).await }).await?;
    cache_put_daily_usage(&cfg.user_id, v.clone());
    Ok(v)
}

// v0.4.6 — Settings UI for provider credentials. Backend lives in
// `provider_creds.rs` (atomic write + cache + mode 0600). Two commands:
// - `get_provider_creds`: returns mask-only view ("Configured" / "Not set"
//   bool per field) plus env-var override flags. Never returns raw secrets.
// - `set_provider_creds`: merges partial update into file; None field =
//   leave unchanged; Some("") = clear that field.

#[derive(Debug, Clone, Serialize)]
struct ProviderCredsView {
    cursor_cookie_set: bool,
    copilot_token_set: bool,
    openrouter_api_key_set: bool,
    /// Optional override for OpenRouter's API URL — NOT secret, returned
    /// plaintext so the Settings UI can show the current custom endpoint
    /// (or empty if using default openrouter.ai).
    openrouter_base_url: Option<String>,
    /// Per-Codex review concern #5: when the user has set an env var AND
    /// also saved a value via the Settings UI, the env var wins (collector
    /// priority is env → file → none). Surface this so the UI can render
    /// the env_override_banner copy. Detected at command-call time, not
    /// at app launch — env vars set after launch by the user's shell are
    /// inherited only by THIS process; we read what we can see.
    env_override_cursor: bool,
    env_override_copilot: bool,
    env_override_openrouter_key: bool,
    env_override_openrouter_url: bool,
    /// v0.4.20 — surface the active storage backend ("os_keychain" or
    /// "file") inside the Settings → Integrations panel itself. v0.4.16
    /// already exposed this on `DiagnosticSnapshot`, but a Linux user
    /// without `libsecret` who never clicks "Copy diagnostic" silently
    /// stays on file storage. Per Gemini 3.1 Pro v0.4.20 review: pair
    /// every silent fallback with a discoverable surface; the diagnostic
    /// copy alone is too easy to miss.
    storage_backend: provider_creds::Backend,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ProviderCredsUpdate {
    /// `None` = leave field unchanged; `Some("")` = explicit clear;
    /// `Some(v)` = set new value.
    cursor_cookie: Option<String>,
    copilot_token: Option<String>,
    openrouter_api_key: Option<String>,
    openrouter_base_url: Option<String>,
}

fn build_provider_creds_view(c: &provider_creds::ProviderCreds) -> ProviderCredsView {
    let env_set = |k: &str| std::env::var(k).is_ok_and(|v| !v.is_empty());
    ProviderCredsView {
        cursor_cookie_set: c.cursor_cookie.as_deref().is_some_and(|s| !s.is_empty()),
        copilot_token_set: c.copilot_token.as_deref().is_some_and(|s| !s.is_empty()),
        openrouter_api_key_set: c
            .openrouter_api_key
            .as_deref()
            .is_some_and(|s| !s.is_empty()),
        openrouter_base_url: c.openrouter_base_url.clone(),
        env_override_cursor: env_set("CURSOR_COOKIE"),
        env_override_copilot: env_set("COPILOT_API_TOKEN"),
        env_override_openrouter_key: env_set("OPENROUTER_API_KEY"),
        env_override_openrouter_url: env_set("OPENROUTER_API_URL"),
        storage_backend: provider_creds::current_backend(),
    }
}

#[tauri::command]
async fn get_provider_creds() -> Result<ProviderCredsView, String> {
    let creds = provider_creds::load().map_err(|e| e.to_string())?;
    Ok(build_provider_creds_view(&creds))
}

#[tauri::command]
async fn set_provider_creds(
    update: ProviderCredsUpdate,
    app: tauri::AppHandle,
) -> Result<ProviderCredsView, String> {
    use tauri::Emitter;
    let mut current = provider_creds::load().map_err(|e| e.to_string())?;
    // For each field: None = leave alone; Some("") = clear; Some(v) = set.
    if let Some(v) = update.cursor_cookie {
        current.cursor_cookie = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.copilot_token {
        current.copilot_token = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.openrouter_api_key {
        current.openrouter_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.openrouter_base_url {
        current.openrouter_base_url = if v.is_empty() { None } else { Some(v) };
    }
    provider_creds::save(&current).map_err(|e| e.to_string())?;
    // Emit event so the background sync loop (or any other listener) can
    // trigger an immediate sync. Frontend doesn't depend on this — the
    // next ~120s tick will pick the new value up regardless. The event
    // exists for the future fast-feedback UX where Settings UI invokes
    // sync_now directly.
    let _ = app.emit("provider_creds_changed", ());
    Ok(build_provider_creds_view(&current))
}

/// v0.4.20 — per-provider snapshot status for the Providers tab error
/// badge. One entry per provider that ran in the last `collect_all`
/// cycle, regardless of success/failure. The frontend reads `ok` and
/// `error` to decide whether to show the red badge.
///
/// Empty Vec on first launch (before the first `collect_all` runs).
/// The frontend treats empty as "no error known yet" — same policy as
/// the v0.4.15 stale indicator.
#[derive(Debug, Serialize)]
struct CollectorStatusView {
    provider: &'static str,
    ok: bool,
    /// Single-line human-readable failure reason, suitable for the
    /// badge tooltip. `None` when the collector returned `Ok` (success
    /// or "user not configured" — both surface as no badge).
    error: Option<String>,
}

#[tauri::command]
fn get_last_collector_status() -> Vec<CollectorStatusView> {
    quota::last_outcomes()
        .into_iter()
        .map(|o| CollectorStatusView {
            provider: o.provider,
            ok: o.snapshot.is_some(),
            error: o.error.map(|e| e.message().to_string()),
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct SyncReport {
    sessions_synced: i64,
    alerts_synced: i64,
    /// v0.3.1: rows the server accepted into daily_usage_metrics for
    /// this device. 0 when the local scan produced no rows or when
    /// helper_sync_daily_usage failed (logged, non-fatal).
    metrics_synced: i64,
    /// v0.3.1: rows the server rejected per-row inside
    /// helper_sync_daily_usage. Non-zero indicates a malformed local
    /// scan entry, not a client-wide failure.
    metrics_errored: i64,
    total_cost_usd: f64,
    total_tokens: i64,
    files_scanned: u32,
    live_sessions_sent: usize,
    live_processes_seen: usize,
    alerts_computed: usize,
}

#[tauri::command]
async fn sync_now(app: tauri::AppHandle) -> Result<SyncReport, String> {
    // v0.4.20 — Tauri-exposed entry point. Wakes the background sync
    // loop (if it's mid-sleep) so the next idle window restarts from
    // "now" rather than continuing the previous 120s countdown — a
    // user who clicks "Refresh now" at second 118 used to get a
    // redundant background tick 2s later. The poke happens ONLY on
    // this path; `background_tick` calls `perform_sync` directly to
    // avoid pokeing itself.
    let report = perform_sync(app).await?;
    poke_manual_refresh();
    Ok(report)
}

async fn perform_sync(app: tauri::AppHandle) -> Result<SyncReport, String> {
    let cfg = config::load()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Device not paired yet".to_string())?;

    // 1. Local scan + live sessions snapshot
    let scan = async_runtime::spawn_blocking(|| scanner::scan(30))
        .await
        .map_err(|e| format!("scanner join error: {e}"))?
        .map_err(|e| e.to_string())?;
    let snapshot = async_runtime::spawn_blocking(sessions::collect_sessions)
        .await
        .map_err(|e| format!("sessions join error: {e}"))?;

    // 2. Compute client-side alerts (budget breach, CPU spike)
    let computed_alerts =
        alerts::compute(&scan, &snapshot, &cfg.thresholds, Some(&cfg.device_name));

    // 3. Fire notification on first budget breach seen today — guarded
    //    by a process-local `Once` so the user isn't buzzed every tick.
    maybe_notify_budget_breach(&app, &computed_alerts);

    let alerts_payload = serde_json::to_value(&computed_alerts).unwrap_or(json!([]));

    // 4a. (v0.4.3, refactored v0.4.20) — best-effort multi-provider
    //     quota scrape. Each of the 6 collectors (Claude, Codex, Cursor,
    //     Gemini, Copilot, OpenRouter) runs concurrently via tokio::spawn
    //     per arm with panic isolation; each returns
    //     `Result<Option<QuotaSnapshot>, CollectorError>`. Failures and
    //     panics are logged per-provider AND cached in
    //     `quota::LAST_OUTCOMES` so the UI can render a red badge on
    //     the affected card via `get_last_collector_status`. Sessions
    //     + alerts upload regardless.
    let outcomes = quota::collect_all().await;
    let mut tier_map = serde_json::Map::with_capacity(outcomes.len());
    let mut remaining_map = serde_json::Map::with_capacity(outcomes.len());
    for outcome in &outcomes {
        if let Some(snap) = &outcome.snapshot {
            // Outer `reset_time` mirrors each provider's headline-tier
            // reset (matches Mac for cross-writer parity per v0.4.2 audit).
            tier_map.insert(
                outcome.provider.to_string(),
                json!({
                    "quota": snap.quota,
                    "remaining": snap.remaining,
                    "plan_type": snap.plan_type,
                    "reset_time": snap.session_reset,
                    "tiers": snap.tiers,
                }),
            );
            remaining_map.insert(outcome.provider.to_string(), json!(snap.remaining));
        }
    }
    let p_provider_remaining = serde_json::Value::Object(remaining_map);
    let p_provider_tiers = serde_json::Value::Object(tier_map);

    // 4b. helper_sync — ship live sessions + computed alerts +
    //     (v0.4.0) provider quota when available.
    let hs = supabase::helper_sync(&supabase::HelperSyncRequest {
        p_device_id: &cfg.device_id,
        p_helper_secret: &cfg.helper_secret,
        p_sessions: sessions::sessions_payload(&snapshot),
        p_alerts: alerts_payload,
        p_provider_remaining,
        p_provider_tiers,
    })
    .await
    .map_err(friendly)?;

    // 5. helper_sync_daily_usage (v0.3.1) — multi-device-aware daily
    //    metrics push. Sibling RPC to helper_sync; uses the same
    //    device credentials. The server derives user_id from
    //    (device_id, helper_secret) so callers can't spoof.
    //
    //    Best-effort: a daily-usage failure shouldn't fail the whole
    //    sync (sessions + alerts already landed via helper_sync). We
    //    log and continue; the next tick retries.
    let metrics: Vec<_> = scan
        .entries
        .iter()
        .filter_map(supabase::DailyUsageMetric::from_entry)
        .collect();
    let (metrics_synced, metrics_errored) = if metrics.is_empty() {
        (0, 0)
    } else {
        match supabase::helper_sync_daily_usage(&supabase::HelperSyncDailyUsageRequest {
            p_device_id: &cfg.device_id,
            p_helper_secret: &cfg.helper_secret,
            p_metrics: metrics,
        })
        .await
        {
            Ok(resp) => (resp.metrics_synced, resp.metrics_errored),
            Err(e) => {
                log::warn!(
                    "helper_sync_daily_usage failed (non-fatal): {}",
                    friendly(e)
                );
                (0, 0)
            }
        }
    };

    Ok(SyncReport {
        sessions_synced: hs.sessions_synced,
        alerts_synced: hs.alerts_synced,
        metrics_synced,
        metrics_errored,
        total_cost_usd: scan.total_cost_usd,
        total_tokens: scan.total_tokens,
        files_scanned: scan.files_scanned,
        live_sessions_sent: snapshot.sessions.len(),
        live_processes_seen: snapshot.total_processes_seen,
        alerts_computed: computed_alerts.len(),
    })
}

/// De-dupe budget-breach toasts so the user gets exactly one popup per
/// budget per day (not one every 2-minute tick). Keyed on the alert's
/// `suppression_key` which already encodes the day.
fn maybe_notify_budget_breach(app: &tauri::AppHandle, alerts: &[alerts::Alert]) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static SEEN: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

    let mut seen = match SEEN.lock() {
        Ok(s) => s,
        Err(_) => return, // poisoned — skip notification rather than panic
    };
    for a in alerts {
        if a.source_kind.as_deref() != Some("budget") {
            continue;
        }
        let key = a.suppression_key.clone().unwrap_or_else(|| a.id.clone());
        if seen.insert(key) {
            notify::budget_breach(app, &a.title, &a.message);
        }
    }
}

fn friendly(e: supabase::SupabaseError) -> String {
    match e {
        supabase::SupabaseError::Rpc { code, message } => {
            format!("{message} [{code}]")
        }
        supabase::SupabaseError::Http { status, body } => {
            // Trim verbose tracebacks in error body
            let snippet: String = body.chars().take(300).collect();
            format!("Supabase HTTP {status}: {snippet}")
        }
        other => other.to_string(),
    }
}

// ------------------------------------------------------------------------
// Background sync — ticks every 2 minutes (same cadence as macOS helper).
// ------------------------------------------------------------------------

/// Threshold at which we notify the user about repeated sync failures.
/// Keeps noise low — transient network blips are normal, but three
/// consecutive failures in 6 minutes means something real is wrong
/// (bad helper_secret, server outage, local clock drift, etc.).
const SYNC_FAILURE_NOTIFY_THRESHOLD: u32 = 3;

/// v0.4.20 — channel sender exposed to the Tauri `sync_now` command.
/// Capacity 1 + drain-before-select discards any signals that fired
/// during the active `background_tick` — only refreshes that hit
/// during the sleep window cause an interval reset. Per Gemini 3.1
/// Pro v0.4.20 review, which correctly flagged that `tokio::sync::Notify`
/// would buffer permits earned during the active tick, then immediately
/// consume them at the top of the next `select!` — i.e. fire a
/// redundant tick right after the manual one, the exact bug we're
/// trying to fix.
static MANUAL_REFRESH_TX: OnceLock<mpsc::Sender<()>> = OnceLock::new();

/// Notify the background sync loop that a manual refresh just ran.
/// If the loop is currently sleeping, the sleep returns early so the
/// next 120s countdown starts from "now". If the loop is mid-tick or
/// the channel is full (signals back up while a tick runs), the
/// drain-before-select on the next iteration discards the signal so
/// the loop doesn't immediately re-tick.
///
/// Best-effort: the channel uses `try_send` so this never blocks
/// `sync_now`, even if the loop hasn't consumed the previous signal
/// yet (capacity 1).
fn poke_manual_refresh() {
    if let Some(tx) = MANUAL_REFRESH_TX.get() {
        let _ = tx.try_send(());
    }
}

/// Outcome of a `wait_for_next_tick` call. v0.4.21 distinguishes the
/// two cases so the loop can SKIP the next `background_tick` when a
/// manual refresh fired the wake — the user's `sync_now` already ran
/// `quota::collect_all`, so an immediate background tick on top is a
/// redundant call. v0.4.20 collapsed both outcomes to `()` and ate the
/// extra tick (VM-flagged as the `+2s spurious entry` in the v0.4.20
/// Block A report).
///
/// v0.4.22 added the `&& !stop` guard at call sites for shutdown-
/// during-Reset-storm safety. v0.4.23 adds a third `Stopped` variant
/// and a `stop`-watching select arm so a stop signal during the 120 s
/// sleep is observed within ~100 ms instead of waiting for the sleep
/// to fully elapse, closing the worst case from 120 s to ~100 ms
/// shutdown latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitOutcome {
    /// `interval` elapsed without interruption — caller should run a
    /// fresh `background_tick`.
    Elapsed,
    /// Manual refresh signal arrived during the idle window — caller
    /// should NOT run a fresh `background_tick` (one just ran via the
    /// manual path) and should re-enter the wait for another full
    /// `interval` of idle.
    Reset,
    /// `stop` flag was raised during the wait — caller should exit
    /// the loop. The outer `while !stop.load(...)` will catch it on
    /// the next iteration, but returning early lets the loop unwind
    /// without burning the rest of the 120 s sleep.
    Stopped,
}

/// Sleep for `interval` OR until the manual-refresh channel signals
/// (whichever comes first). Drains buffered signals first so any
/// `poke_manual_refresh` call that landed during the previous
/// `background_tick` is discarded — the user already got their result
/// via the manual `sync_now` path; we only want to react to clicks
/// that happen while we're idle.
///
/// Returns `Elapsed` on a clean 120 s sleep, `Reset` when a manual
/// refresh interrupted, `Stopped` when the `stop` flag was raised
/// during the wait. Per v0.4.20 VM Block A: the caller MUST treat
/// `Reset` as "skip the next tick AND wait again" — otherwise a
/// redundant `background_tick` fires immediately after the user's
/// manual sync. Use the `while … == Reset && !stop` pattern at the
/// call site.
///
/// The `Some(_) = rx.recv()` pattern (vs the bare `_ = rx.recv()`)
/// is load-bearing: when `MANUAL_REFRESH_TX.set(tx)` fails (e.g. a
/// hot-reload spawn raced and the static is already populated), the
/// LOCAL `tx` is dropped, closing the channel, and `recv()` returns
/// `None` instantly. With the bare pattern, `select!` would fire the
/// recv arm immediately → return `Reset` → caller's `while` loops →
/// recv() returns None again → busy loop at 100 % CPU. With the
/// `Some(_)` pattern, the arm is disabled when `recv()` returns
/// `None`, so `select!` falls through to the sleep arm and the loop
/// continues normally (just without manual-refresh wake capability).
/// Per Gemini 3.1 Pro v0.4.21 review P1.
///
/// v0.4.23: takes `stop: &AtomicBool` and adds a third select arm
/// that polls the flag every 100 ms. When the app is shutting down,
/// the loop exits within ~100 ms of `stop.store(true)` instead of
/// waiting for the current 120 s sleep to elapse. The polling cost
/// is ~10 atomic loads per second per loop instance — negligible.
///
/// Extracted as `pub(crate)` for unit testing — see `mod tests` below.
pub(crate) async fn wait_for_next_tick(
    rx: &mut mpsc::Receiver<()>,
    interval: Duration,
    stop: &AtomicBool,
) -> WaitOutcome {
    while rx.try_recv().is_ok() {}
    // Fast path: if stop was set BEFORE we entered the wait (e.g.
    // the previous tick finished after stop was raised), return
    // immediately rather than starting a 100 ms poll cycle.
    if stop.load(Ordering::Relaxed) {
        return WaitOutcome::Stopped;
    }
    tokio::select! {
        _ = tokio::time::sleep(interval) => WaitOutcome::Elapsed,
        Some(_) = rx.recv() => {
            log::debug!("background tick reset by manual refresh during idle window");
            WaitOutcome::Reset
        }
        _ = poll_stop_signal(stop) => WaitOutcome::Stopped,
    }
}

/// Polls `stop` every 100 ms until it flips to `true`, then returns.
/// 100 ms granularity is plenty for human-perceptible shutdown
/// responsiveness (a 100 ms delay closing an app feels instant).
///
/// Uses `tokio::time::interval` rather than allocating a fresh
/// `Sleep` future per iteration — Gemini 3.1 Pro v0.4.23 review P2.
/// `interval` reuses one timer slot in the timer wheel, so the cost
/// is one register-on-entry plus self-rearming wakeups instead of
/// register/deregister cycles. Negligible either way; this is the
/// idiomatic tokio shape.
///
/// (Considered switching `stop: AtomicBool` to
/// `tokio_util::sync::CancellationToken` for true cancel-arm
/// semantics — Gemini's preferred approach. Deferred because the
/// type signature ripples through `run()`, `spawn_background_sync`,
/// and 4 tests; v0.4.23's scope is closed at "make stop observable
/// within 100 ms," not "rewrite the cancellation primitive." Track
/// for a later sprint.)
async fn poll_stop_signal(stop: &AtomicBool) {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    // First tick fires immediately at `interval.tick().await`; skip
    // it so we always sleep at least one cadence before re-checking
    // (the fast-path in `wait_for_next_tick` already handled the
    // "stop true at entry" case).
    interval.tick().await;
    while !stop.load(Ordering::Relaxed) {
        interval.tick().await;
    }
}

fn spawn_background_sync(app: tauri::AppHandle, stop: Arc<AtomicBool>) {
    let (tx, mut rx) = mpsc::channel::<()>(1);
    // It's OK if `set` fails (e.g. the spawn somehow happens twice in
    // a hot-reload dev cycle) — the second call's channel just isn't
    // wired to the loop, so manual pokes from `sync_now` are no-ops
    // until next launch. We don't panic.
    let _ = MANUAL_REFRESH_TX.set(tx);

    async_runtime::spawn(async move {
        log::info!(
            "Background sync loop started — first tick in 20s, then every {}s",
            SYNC_INTERVAL.as_secs()
        );
        // First tick after 20s so the UI feels responsive on startup
        // without racing the initial human pairing flow.
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut consecutive_failures: u32 = 0;
        // v0.4.23 — single retry-after-backoff on transient 5xx /
        // network errors. `tick_attempt = 0` is the first attempt of
        // a 120 s cycle; `1` means we already retried once and the
        // next failure should fall through to the streak counter.
        // Reset on both Ok branches AND on non-transient Err so the
        // next 120 s cycle starts fresh.
        let mut tick_attempt: u32 = 0;
        while !stop.load(Ordering::Relaxed) {
            match background_tick(&app).await {
                Ok(Some(report)) => {
                    log::info!(
                        "background sync ok — {} sessions, {} alerts, {} metrics ({} errored)",
                        report.sessions_synced,
                        report.alerts_synced,
                        report.metrics_synced,
                        report.metrics_errored
                    );
                    consecutive_failures = 0;
                    tick_attempt = 0;
                }
                Ok(None) => {
                    log::debug!("background sync skipped — not paired");
                    tick_attempt = 0;
                }
                // v0.4.23 — transient HTTP 5xx / network blip: retry
                // ONCE after 5 s before counting toward the streak. A
                // single Anthropic 503 should not cause a "2× consecutive"
                // log line on the very next 120 s tick. Per the autonomy
                // contract, sync_failure_streak fires the desktop
                // notification at threshold — false positives are
                // expensive, so retry first, count second.
                Err(e) if tick_attempt == 0 && looks_like_transient_5xx(&e) => {
                    log::info!("transient HTTP error ({e}) — retrying once in 5 s before counting");
                    tick_attempt = 1;
                    // Stop-responsive sleep: a stop raised during the
                    // 5 s retry window breaks the sleep early.
                    // Without this select, the 5 s retry would
                    // inherit its own shutdown latency and undermine
                    // the ~100 ms stop guarantee Item 1 made. Per
                    // Gemini 3.1 Pro v0.4.23 review P3.
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                        _ = poll_stop_signal(&stop) => {}
                    }
                    continue;
                }
                Err(e) => {
                    tick_attempt = 0;
                    consecutive_failures += 1;
                    log::warn!(
                        "background sync error ({}× consecutive): {e}",
                        consecutive_failures
                    );
                    // v0.3.0 — on the first auth-shaped failure, ask the
                    // server whether the device or account is still
                    // healthy. If gone, clear local credentials and stop
                    // sync until the user signs in again.
                    if looks_like_auth_failure(&e) {
                        match classify_auth_failure(&app).await {
                            Ok(classified) if classified => {
                                // Local state cleared; reset the streak counter
                                // so we don't spam another notification when the
                                // (now-no-op) tick keeps not-syncing.
                                consecutive_failures = 0;
                                // v0.4.21 — keep waiting on Reset so a manual
                                // refresh during the post-clear idle window
                                // doesn't trigger an immediate (now-no-op) tick.
                                // `&& !stop` keeps shutdown responsive even
                                // under repeated manual clicks (Gemini P2).
                                // v0.4.23 — `wait_for_next_tick` also returns
                                // `Stopped` for fast shutdown; `Stopped == Reset`
                                // is false so the existing loop naturally exits.
                                while wait_for_next_tick(&mut rx, SYNC_INTERVAL, &stop).await
                                    == WaitOutcome::Reset
                                    && !stop.load(Ordering::Relaxed)
                                {
                                }
                                continue;
                            }
                            Ok(_) => {
                                // Server says healthy → 401 was transient.
                                // Fall through to the streak-counter path.
                            }
                            Err(probe_err) => {
                                log::warn!("device_status probe failed: {probe_err}");
                            }
                        }
                    }
                    if consecutive_failures == SYNC_FAILURE_NOTIFY_THRESHOLD {
                        notify::sync_failure_streak(&app, consecutive_failures, &e);
                    }
                }
            }
            // v0.4.21 — `Reset` means "user clicked Refresh now, which
            // already ran a manual `quota::collect_all` via `sync_now`".
            // Loop back into another full-interval wait instead of
            // falling through to a redundant `background_tick` at the
            // top of the next iteration. Multiple clicks in succession
            // each defer the next tick by another 120 s — desired.
            // `&& !stop` (Gemini P2 catch): without it, the inner loop
            // can outlive a shutdown request indefinitely if the user
            // keeps clicking faster than 120 s.
            // v0.4.23 — `wait_for_next_tick` also returns `Stopped` on
            // shutdown, so the loop exits within ~100 ms instead of
            // waiting for the current 120 s sleep to elapse.
            while wait_for_next_tick(&mut rx, SYNC_INTERVAL, &stop).await == WaitOutcome::Reset
                && !stop.load(Ordering::Relaxed)
            {}
        }
    });
}

/// Heuristic: does the sync error look like the device credential was
/// rejected? `friendly` formats Supabase HTTP errors as
/// `"Supabase HTTP {status}: ..."` — we look for 401 / 403 explicitly.
/// This is intentionally narrow: we don't want a 500 / 502 to trigger
/// a `device_status` probe.
fn looks_like_auth_failure(msg: &str) -> bool {
    msg.contains("HTTP 401")
        || msg.contains("HTTP 403")
        || msg.contains("Device not found or unauthorized")
}

/// v0.4.23 — heuristic: does the sync error look like a transient
/// network / upstream blip worth a single retry? Covers the common
/// cases where a one-shot 503 from Anthropic, a TCP reset, or a DNS
/// hiccup produces an `Err` that would otherwise immediately count
/// toward the consecutive-failures streak.
///
/// Intentionally conservative: only retries on signals that almost
/// certainly resolve themselves within seconds. 4xx / auth failures
/// are NOT retried — those are real, user-actionable conditions.
///
/// All matching is case-insensitive (Gemini 3.1 Pro v0.4.23 review
/// P3): some upstream proxies / SDKs lowercase the `HTTP` prefix or
/// embed status codes inside larger messages with arbitrary casing.
fn looks_like_transient_5xx(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("http 500")
        || lower.contains("http 502")
        || lower.contains("http 503")
        || lower.contains("http 504")
        || lower.contains("http 429")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("timed out")
        || lower.contains("network is unreachable")
        || lower.contains("temporary failure in name resolution")
}

/// Probe `device_status` and act on the response. Returns `Ok(true)`
/// when the local pairing was cleared (device or account gone), so the
/// caller can reset its streak counter.
async fn classify_auth_failure(app: &tauri::AppHandle) -> Result<bool, String> {
    let cfg = match config::load().map_err(|e| e.to_string())? {
        Some(c) => c,
        None => return Ok(false),
    };
    let req = supabase::DeviceStatusRequest {
        p_device_id: &cfg.device_id,
        p_helper_secret: &cfg.helper_secret,
    };
    let status = supabase::device_status(&req).await.map_err(friendly)?;
    match status {
        supabase::DeviceStatus::Healthy => Ok(false),
        supabase::DeviceStatus::DeviceMissing => {
            log::warn!("device_status: device_missing — clearing local pairing");
            let _ = keychain::delete_refresh_token();
            let _ = config::clear();
            cache_invalidate();
            notify::session_expired(app, "device_missing");
            Ok(true)
        }
        supabase::DeviceStatus::AccountMissing => {
            log::warn!("device_status: account_missing — clearing local pairing");
            let _ = keychain::delete_refresh_token();
            let _ = config::clear();
            cache_invalidate();
            notify::session_expired(app, "account_missing");
            Ok(true)
        }
    }
}

async fn background_tick(app: &tauri::AppHandle) -> Result<Option<SyncReport>, String> {
    // If we're not paired, this is a no-op.
    let cfg_exists = config::load().map_err(|e| e.to_string())?.is_some();
    if !cfg_exists {
        return Ok(None);
    }
    // v0.4.20 — call `perform_sync` directly (NOT the Tauri-exposed
    // `sync_now`) so we don't fire `poke_manual_refresh` against
    // ourselves. Pokeing from inside the background loop would create
    // a self-feedback edge case: the manual-refresh channel has
    // capacity 1, so a self-poke could displace a real user click
    // that arrived during the same tick.
    let report = perform_sync(app.clone()).await?;
    Ok(Some(report))
}

// ------------------------------------------------------------------------
// Tauri entry
// ------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // env_logger was replaced by tauri-plugin-log in v0.3.4 — see the
    // Builder chain below. Logging now writes to the OS log dir on top
    // of stdout, which Win release builds were previously discarding
    // entirely (no console attached → stderr → /dev/null). VM E2E
    // 2026-05-02 confirmed users had nothing on disk to attach to bug
    // reports.

    // Sentry — no-op when CLI_PULSE_SENTRY_DSN is unset (the default).
    // Install before tauri::Builder so the panic handler is registered
    // for the lifetime of the process.
    sentry_init::install();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_bg = stop.clone();

    tauri::Builder::default()
        // v0.3.4 — single-instance must be the FIRST plugin so the
        // second-launch handler fires before any other initialization.
        // The handler raises the existing main window via the AppHandle.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            use tauri::Manager;
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        // v0.3.4 — file logging. Default OS log dir
        // (Win: %LOCALAPPDATA%\dev.clipulse.desktop\logs\,
        //  macOS: ~/Library/Logs/dev.clipulse.desktop/,
        //  Linux: ~/.local/share/dev.clipulse.desktop/logs/)
        // Rotation: keep up to 5 files at 5 MB each = ~25 MB cap.
        .plugin(
            tauri_plugin_log::Builder::default()
                .level(log::LevelFilter::Info)
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("cli-pulse".into()),
                    }),
                ])
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepAll)
                .max_file_size(5 * 1024 * 1024)
                .timezone_strategy(tauri_plugin_log::TimezoneStrategy::UseLocal)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(move |app| {
            // v0.3.5 — write a guaranteed-flushed startup banner so users
            // always have SOMETHING in the log file regardless of paired
            // state. v0.3.4 VM E2E found the log file was 0 bytes for an
            // unpaired desktop because background_tick at debug level
            // gets filtered, and Sentry init only logs when DSN is set.
            // The banner runs after all plugins have installed, so the
            // logger is guaranteed live.
            use tauri::Manager;
            log::info!(
                "CLI Pulse Desktop v{} starting on {} ({})",
                HELPER_VERSION,
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            if let Ok(dir) = app.path().app_log_dir() {
                log::info!("Log directory: {}", dir.display());
            }
            match config::load() {
                Ok(Some(cfg)) => log::info!(
                    "Paired (device {}…)",
                    &cfg.device_id.chars().take(8).collect::<String>()
                ),
                _ => log::info!("Not paired — sign in via Settings to start syncing"),
            }
            // v0.4.16 — initialize provider-creds storage backend (OS
            // keychain primary, file fallback) and run the one-shot
            // v1->v2 migration if a plaintext provider_creds.json
            // exists. Per Gemini 3.1 Pro review: at startup, NOT on
            // first save() — otherwise users who never edit creds
            // stay on the plaintext file forever.
            provider_creds::init_backend();
            spawn_background_sync(app.handle().clone(), stop_bg.clone());
            // System tray — Windows first-class, Linux works with AppIndicator
            // when libayatana-appindicator3 is installed, otherwise we log and
            // continue window-first.
            if let Err(e) = tray::install(app.handle()) {
                log::warn!("tray init failed (continuing without tray): {e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            scan_usage,
            list_sessions,
            pair_device,
            unpair_device,
            sync_now,
            get_thresholds,
            set_thresholds,
            preview_alerts,
            diagnostic_snapshot,
            // v0.3.0 — direct email OTP sign-in
            auth_send_otp,
            auth_verify_otp,
            auth_status,
            auth_sign_out,
            auth_account_check,
            auth_default_device_name,
            // v0.3.4 — user-scoped dashboard reads
            get_dashboard_summary,
            get_provider_summary,
            get_daily_usage,
            // v0.4.6 — Settings UI for provider credentials
            get_provider_creds,
            set_provider_creds,
            // v0.4.20 — per-provider collect status for UI error badge
            get_last_collector_status,
            // v0.4.22 — Settings → About diagnostic Sentry-ingestion test
            emit_test_sentry_event,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    // Let the background loop exit cleanly on app shutdown.
    stop.store(true, Ordering::Relaxed);
}

// silence `Value` / `json` unused if not referenced elsewhere
#[allow(dead_code)]
fn _json_ref_placeholder(_v: Value) {}

#[cfg(test)]
mod tests {
    //! v0.4.20 — `wait_for_next_tick` mpsc-channel sleep with manual-
    //! refresh interrupt.
    //!
    //! Two cases pin the contract:
    //!
    //! 1. Drain semantics. A signal that arrived during the previous
    //!    `background_tick` (i.e. before we entered the wait) must be
    //!    discarded so we sleep the full interval. Without the drain,
    //!    `tokio::sync::Notify` would buffer the permit and the next
    //!    `select!` would fire it instantly — re-running a redundant
    //!    background tick right after the manual one. This is the
    //!    exact bug Gemini 3.1 Pro flagged in the v0.4.20 review.
    //!
    //! 2. Idle interrupt. A signal that arrives while we ARE in the
    //!    sleep window (the user clicks "Refresh now" between two
    //!    background ticks) must wake the sleep early.
    //!
    //! Both tests run with `start_paused = true` so virtual time
    //! advances when all tasks block — no real wall-clock wait.

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn drained_signal_does_not_interrupt_idle_sleep() {
        let (tx, mut rx) = mpsc::channel::<()>(1);
        // Simulate the regression scenario: a `poke_manual_refresh()`
        // fired DURING the prior `background_tick` and is buffered in
        // the channel when we enter the wait.
        tx.send(()).await.unwrap();

        let interval = Duration::from_secs(120);
        let stop = AtomicBool::new(false);
        let started = tokio::time::Instant::now();
        let outcome = wait_for_next_tick(&mut rx, interval, &stop).await;
        let elapsed = started.elapsed();

        assert_eq!(
            elapsed, interval,
            "drained pre-wait signal must NOT shorten the idle sleep — \
             that's the exact regression Gemini caught in the v0.4.20 review"
        );
        assert_eq!(
            outcome,
            WaitOutcome::Elapsed,
            "drained pre-wait signal must yield Elapsed (not Reset) — \
             the loop should run a fresh background_tick after this"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn signal_during_idle_window_wakes_sleep_early() {
        let (tx, mut rx) = mpsc::channel::<()>(1);
        let interval = Duration::from_secs(120);
        let click_at = Duration::from_secs(30);
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop_for_waiter = stop.clone();

        let waiter = tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let outcome = wait_for_next_tick(&mut rx, interval, &stop_for_waiter).await;
            (started.elapsed(), outcome)
        });

        tokio::time::sleep(click_at).await;
        tx.send(()).await.unwrap();

        let (elapsed, outcome) = waiter.await.unwrap();
        assert_eq!(
            elapsed, click_at,
            "manual refresh fired during the idle window must wake the sleep at click time"
        );
        assert_eq!(
            outcome,
            WaitOutcome::Reset,
            "manual refresh fired during the idle window must yield Reset"
        );
    }

    /// v0.4.23 — stop signal raised during the 120 s sleep should be
    /// observed within ~100 ms (the poll cadence) instead of waiting
    /// for the full sleep to elapse. Closes the worst-case shutdown
    /// latency from 120 s to ~100 ms.
    #[tokio::test(start_paused = true)]
    async fn stop_signal_during_idle_returns_stopped_promptly() {
        let (_tx, mut rx) = mpsc::channel::<()>(1);
        let interval = Duration::from_secs(120);
        let stop_at = Duration::from_secs(30);
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop_for_setter = stop.clone();
        let stop_for_waiter = stop.clone();

        let waiter = tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let outcome = wait_for_next_tick(&mut rx, interval, &stop_for_waiter).await;
            (started.elapsed(), outcome)
        });

        tokio::time::sleep(stop_at).await;
        stop_for_setter.store(true, Ordering::Relaxed);

        let (elapsed, outcome) = waiter.await.unwrap();
        assert_eq!(
            outcome,
            WaitOutcome::Stopped,
            "stop flag during idle must yield WaitOutcome::Stopped"
        );
        // The poll cadence is 100 ms; we tolerate up to ~150 ms slack
        // because virtual time can advance multiple poll iterations
        // between the setter's store and the next poll-loop wake.
        assert!(
            elapsed >= stop_at && elapsed <= stop_at + Duration::from_millis(150),
            "stop should be observed within ~100 ms of being set, got {elapsed:?}"
        );
    }

    /// v0.4.23 — fast-path: if `stop` is already true when
    /// `wait_for_next_tick` is entered (e.g. the previous `background_tick`
    /// took long enough that `stop` got set in the meantime), the fn
    /// should return `Stopped` immediately, NOT wait for the 100 ms
    /// poll cycle.
    #[tokio::test(start_paused = true)]
    async fn stop_already_set_at_entry_returns_stopped_immediately() {
        let (_tx, mut rx) = mpsc::channel::<()>(1);
        let interval = Duration::from_secs(120);
        let stop = AtomicBool::new(true);

        let started = tokio::time::Instant::now();
        let outcome = wait_for_next_tick(&mut rx, interval, &stop).await;
        let elapsed = started.elapsed();

        assert_eq!(outcome, WaitOutcome::Stopped);
        assert!(
            elapsed < Duration::from_millis(50),
            "fast-path must skip the 100 ms poll cycle, got {elapsed:?}"
        );
    }

    /// v0.4.21 fix-to-the-fix. v0.4.20 VM Block A measured a +2 s
    /// `quota::collect_all` entry immediately after a manual click —
    /// the `wait_for_next_tick` woke up early (correct), then the
    /// loop fell through and ran `background_tick` at the top of the
    /// next iteration (NOT correct — the user's `sync_now` already
    /// ran one). This test simulates the loop using a counter in
    /// place of `background_tick` and asserts that a manual click
    /// during the idle window does NOT increment the tick counter
    /// until a full `interval` has actually elapsed since the click.
    #[tokio::test(start_paused = true)]
    async fn manual_refresh_does_not_cause_extra_tick() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        let (tx, mut rx) = mpsc::channel::<()>(1);
        let counter = std::sync::Arc::new(AtomicUsize::new(0));
        let stop = std::sync::Arc::new(AtomicBool::new(false));

        let counter_for_loop = counter.clone();
        let stop_for_loop = stop.clone();

        let interval = Duration::from_secs(120);
        let click_at = Duration::from_secs(60);

        let loop_handle = tokio::spawn(async move {
            // Mirror of `spawn_background_sync`'s body, with a counter
            // standing in for `background_tick`. Mirrors production's
            // `while … == Reset && !stop {}` pattern (the `&& !stop`
            // half is the P2 fix from Gemini's v0.4.21 review).
            // v0.4.23 — `wait_for_next_tick` now also takes `&stop`
            // and can return `Stopped`; mirror the production signature.
            while !stop_for_loop.load(AtomicOrdering::Relaxed) {
                counter_for_loop.fetch_add(1, AtomicOrdering::SeqCst);
                while wait_for_next_tick(&mut rx, interval, &stop_for_loop).await
                    == WaitOutcome::Reset
                    && !stop_for_loop.load(AtomicOrdering::Relaxed)
                {}
            }
        });

        // Let the initial tick land.
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(
            counter.load(AtomicOrdering::SeqCst),
            1,
            "initial tick must fire at startup"
        );

        // Mid-idle: simulate a "Refresh now" click.
        tokio::time::sleep(click_at).await;
        tx.send(()).await.unwrap();

        // Give the wait helper a virtual moment to react.
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(
            counter.load(AtomicOrdering::SeqCst),
            1,
            "manual refresh during idle window must NOT trigger an extra background_tick — \
             that's the v0.4.20 Block A regression VM caught at +2 s"
        );

        // Advance to one full `interval` after the click — next tick
        // should fire here, not earlier.
        tokio::time::sleep(interval - Duration::from_millis(10)).await;
        assert_eq!(
            counter.load(AtomicOrdering::SeqCst),
            1,
            "next tick must NOT fire earlier than `interval` after the manual click"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            counter.load(AtomicOrdering::SeqCst),
            2,
            "next tick MUST fire at `click_time + interval` (not the original schedule)"
        );

        // Cleanup. With v0.4.21's Gemini P1 fix (`Some(_) = rx.recv()`),
        // dropping `tx` is now safe — `recv()` returning `None`
        // disables that select arm instead of falling through to
        // `Reset`. The pending sleep arm runs to completion, returns
        // `Elapsed`, the inner `while … == Reset && !stop` exits, and
        // the outer `while !stop` exits. Confirms the P1 fix end-to-
        // end without resorting to `loop_handle.abort()`.
        stop.store(true, AtomicOrdering::Relaxed);
        drop(tx);
        // Advance virtual time so the pending sleep inside
        // `wait_for_next_tick` completes naturally.
        tokio::time::sleep(interval + Duration::from_millis(10)).await;
        // Should now exit cleanly. If this hangs, the P1 / P2 fix has
        // regressed.
        loop_handle.await.expect("loop task should exit cleanly");
    }

    /// v0.4.23 — `looks_like_transient_5xx` powers the "retry once
    /// before counting toward the streak" branch. Real transient
    /// shapes seen in the field: Anthropic 503, Supabase 502 during
    /// deploys, DNS resolution failures during VPN reconnects.
    #[test]
    fn looks_like_transient_5xx_matches_common_shapes() {
        assert!(looks_like_transient_5xx(
            "Supabase HTTP 503: service temporarily unavailable"
        ));
        assert!(looks_like_transient_5xx(
            "Anthropic API HTTP 502: bad gateway"
        ));
        assert!(looks_like_transient_5xx(
            "HTTP 504 Gateway Timeout from upstream"
        ));
        assert!(looks_like_transient_5xx("HTTP 500 internal server error"));
        assert!(looks_like_transient_5xx(
            "HTTP 429: Too many requests, retry-after 5s"
        ));
        assert!(looks_like_transient_5xx(
            "Connection reset by peer (os error 54)"
        ));
        assert!(looks_like_transient_5xx("connection refused"));
        assert!(looks_like_transient_5xx(
            "Operation timed out after 30 seconds"
        ));
        assert!(looks_like_transient_5xx(
            "Network is unreachable (os error 51)"
        ));
        assert!(looks_like_transient_5xx(
            "Temporary failure in name resolution"
        ));
    }

    #[test]
    fn looks_like_transient_5xx_rejects_4xx_and_local() {
        // 4xx is real, user-actionable — don't retry.
        assert!(!looks_like_transient_5xx("HTTP 401 Unauthorized"));
        assert!(!looks_like_transient_5xx("HTTP 403 Forbidden"));
        assert!(!looks_like_transient_5xx("HTTP 404 Not Found"));
        assert!(!looks_like_transient_5xx(
            "Device not found or unauthorized"
        ));
        // Local issues — JSON parse, file IO, panic — also not retried.
        assert!(!looks_like_transient_5xx(
            "JSON: key must be a string at line 1 column 3"
        ));
        assert!(!looks_like_transient_5xx(
            "atomic write to /Users/x/.gemini/oauth_creds.json failed"
        ));
        assert!(!looks_like_transient_5xx(
            "collector panicked: thread 'tokio-runtime-worker' panicked"
        ));
        // Generic non-error message must not match.
        assert!(!looks_like_transient_5xx(
            "background sync ok — 12 sessions, 0 alerts"
        ));
    }
}
