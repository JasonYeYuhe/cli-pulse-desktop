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
pub mod scanner;
pub mod sentry_init;
pub mod sessions;
pub mod supabase;
pub mod tray;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use config::HelperConfig;
use once_cell::sync::Lazy;
use scanner::ScanResult;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::async_runtime;

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
}

/// Used by the About panel to render a copyable diagnostic block when
/// users report issues. Avoids leaking the full helper_secret or
/// user_id — only the first 8 chars of device_id are exposed.
#[tauri::command]
fn diagnostic_snapshot() -> Result<DiagnosticSnapshot, String> {
    let cfg = config::load().map_err(|e| e.to_string())?;
    let cache_dir =
        cache::cache_path("codex", None).and_then(|p| p.parent().map(|d| d.display().to_string()));
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
    })
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

#[tauri::command]
fn unpair_device() -> Result<(), String> {
    // Best-effort: drop the keychain refresh token alongside the helper
    // config. Either failure is non-fatal — the goal is to leave the
    // device in a clean state.
    let _ = keychain::delete_refresh_token();
    config::clear().map_err(|e| e.to_string())
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

#[derive(Debug, Serialize)]
struct SyncReport {
    sessions_synced: i64,
    alerts_synced: i64,
    total_cost_usd: f64,
    total_tokens: i64,
    files_scanned: u32,
    live_sessions_sent: usize,
    live_processes_seen: usize,
    alerts_computed: usize,
}

#[tauri::command]
async fn sync_now(app: tauri::AppHandle) -> Result<SyncReport, String> {
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

    // 4. helper_sync — ship live sessions + computed alerts
    let hs = supabase::helper_sync(&supabase::HelperSyncRequest {
        p_device_id: &cfg.device_id,
        p_helper_secret: &cfg.helper_secret,
        p_sessions: sessions::sessions_payload(&snapshot),
        p_alerts: alerts_payload,
        p_provider_remaining: json!({}),
        p_provider_tiers: json!({}),
    })
    .await
    .map_err(friendly)?;

    // Daily-usage upload (the previous `upsert_daily_usage` step) is removed
    // in v0.2.14. The RPC required a user JWT but Tauri only has the helper's
    // anon-key credentials, so every call returned an error and bubbled up as
    // a sync failure even when sessions+alerts had landed. v0.3.1 routes
    // daily metrics through a multi-device-aware path.

    Ok(SyncReport {
        sessions_synced: hs.sessions_synced,
        alerts_synced: hs.alerts_synced,
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

fn spawn_background_sync(app: tauri::AppHandle, stop: Arc<AtomicBool>) {
    async_runtime::spawn(async move {
        // First tick after 20s so the UI feels responsive on startup
        // without racing the initial human pairing flow.
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut consecutive_failures: u32 = 0;
        while !stop.load(Ordering::Relaxed) {
            match background_tick(&app).await {
                Ok(Some(report)) => {
                    log::info!(
                        "background sync ok — {} sessions, {} alerts",
                        report.sessions_synced,
                        report.alerts_synced
                    );
                    consecutive_failures = 0;
                }
                Ok(None) => {
                    log::debug!("background sync skipped — not paired");
                }
                Err(e) => {
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
                                tokio::time::sleep(SYNC_INTERVAL).await;
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
            tokio::time::sleep(SYNC_INTERVAL).await;
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
            notify::session_expired(app, "device_missing");
            Ok(true)
        }
        supabase::DeviceStatus::AccountMissing => {
            log::warn!("device_status: account_missing — clearing local pairing");
            let _ = keychain::delete_refresh_token();
            let _ = config::clear();
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
    let report = sync_now(app.clone()).await?;
    Ok(Some(report))
}

// ------------------------------------------------------------------------
// Tauri entry
// ------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Sentry — no-op when CLI_PULSE_SENTRY_DSN is unset (the default).
    // Install before tauri::Builder so the panic handler is registered
    // for the lifetime of the process.
    sentry_init::install();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_bg = stop.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(move |app| {
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    // Let the background loop exit cleanly on app shutdown.
    stop.store(true, Ordering::Relaxed);
}

// silence `Value` / `json` unused if not referenced elsewhere
#[allow(dead_code)]
fn _json_ref_placeholder(_v: Value) {}
