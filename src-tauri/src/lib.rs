//! CLI Pulse Desktop — Tauri backend entry point.
//!
//! Sprint 0: local JSONL scan + per-day/model/provider aggregation.
//! Sprint 1: Supabase pairing, config persistence, helper_sync
//! round-trips, periodic 2-minute sync tick.

pub mod alerts;
pub mod auth;
pub mod cache;
pub mod config;
pub mod cost_forecast;
pub mod crash_recovery;
pub mod creds;
pub mod cwd_hmac;
pub mod diagnostic_bundle;
pub mod install_hook;
pub mod keychain;
pub mod machine;
pub mod notify;
pub mod paths;
pub mod pricing;
pub mod provider_creds;
pub mod quota;
pub mod redaction;
// v0.8.0 introduced a `remote::*` module tree (transport / agent /
// events / log) for the ConPTY managed-session host. v0.8.1 reverts
// the ConPTY feature; only `remote::log` survives, used by
// `bin/remote_hook.rs` for the diagnostic file appender that closed
// the v0.7.0 blind spot per
// `feedback_remote_hook_diagnostic_blind_spot.md`.
pub mod remote;
pub mod risk;
pub mod scanner;
pub mod sentry_init;
pub mod service_status;
pub mod sessions;
pub mod smoke;
pub mod supabase;
pub mod top_projects;
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
/// v0.5.6 — tray menu mini-metrics refresh cadence. Independent of
/// `SYNC_INTERVAL` so future tuning to either doesn't accidentally
/// couple them; both happen to be 120 s today which avoids racing
/// the existing 30 s-TTL `DASHBOARD_CACHE`. Per Codex pre-
/// implementation review: tray must NEVER force a fresh fetch — it
/// reads cached values only, leaving the user-driven UI fetches in
/// charge of cache population.
const TRAY_REFRESH_INTERVAL: Duration = Duration::from_secs(120);

/// v0.5.6 — wall-clock timestamp of the most recent successful
/// `background_tick` (or sync_now). The tray menu's "Synced N ago"
/// row reads this to compute the relative age. Updated in
/// `perform_sync` on the success path; never decremented or reset
/// (a sync can only be more recent than its previous value).
static LAST_SUCCESSFUL_SYNC_AT: Lazy<std::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>> =
    Lazy::new(|| std::sync::Mutex::new(None));

/// v0.5.7 — local-scan-derived (month_so_far_usd, predicted_month_total_usd)
/// snapshot. Computed at the end of every successful `perform_sync` from
/// THIS device's scanner output, then read by the tray refresh loop as
/// the primary data source.
///
/// Why this exists: the v0.5.6 tray read `cache_get_daily_usage`
/// (DASHBOARD_CACHE.daily_usage, 30 s TTL) which is only populated by
/// the Overview tab's `CostForecastCard` polling at 60 s. When the user
/// minimizes to use the tray (the natural workflow!), the Overview
/// component unmounts, polling stops, the cache expires, and every
/// subsequent 120 s tray tick reads None → renders "—" forever. VM
/// verify on 2026-05-06 caught this as a P2: tray's "Synced N ago"
/// updated correctly while Month / Forecast lines stayed at em-dash.
///
/// The fix is to derive the values from local scanner data — which the
/// background tick refreshes every 120 s anyway. This is "this device's
/// month-to-date" which equals the cross-device sum for single-device
/// users, and is a meaningful subset for multi-device users. Trade-off
/// adopted: tray accuracy never reaches "—" for paired users with any
/// local activity, at the cost of slight under-counting for multi-
/// device users (whose dashboard view still shows the full sum).
static LAST_LOCAL_TRAY_VALUES: Lazy<std::sync::Mutex<Option<(f64, f64)>>> =
    Lazy::new(|| std::sync::Mutex::new(None));

/// Compute and stash (month_so_far, predicted_total) from local scan
/// entries. Called from `perform_sync`'s success path so the tray
/// always has fresh local-derived numbers after T+~20 s (first
/// background_tick) and every 120 s thereafter.
fn record_local_tray_snapshot(scan: &scanner::ScanResult) {
    // Re-shape scanner DailyEntries into the DailyUsageRow shape the
    // forecast helper expects. Drops the synthetic CLAUDE_MSG_BUCKET
    // model rows — `forecast_from_daily` only cares about cost, but
    // including the bucket would double-count Claude messages against
    // their cost contribution (the bucket has cost=None / 0.0 in
    // practice, but explicit filtering keeps the contract clear).
    let local_daily: Vec<supabase::DailyUsageRow> = scan
        .entries
        .iter()
        .filter(|e| e.model != scanner::CLAUDE_MSG_BUCKET_MODEL)
        .map(|e| supabase::DailyUsageRow {
            metric_date: e.date.clone(),
            provider: e.provider.clone(),
            model: e.model.clone(),
            input_tokens: e.input_tokens,
            cached_tokens: e.cached_tokens,
            output_tokens: e.output_tokens,
            cost: e.cost_usd.unwrap_or(0.0),
        })
        .collect();
    let today = chrono::Local::now().date_naive();
    if let Some(forecast) = cost_forecast::forecast_from_daily(&local_daily, today) {
        if let Ok(mut g) = LAST_LOCAL_TRAY_VALUES.lock() {
            *g = Some((forecast.actual_to_date, forecast.predicted_month_total));
        }
    }
}

fn last_local_tray_values() -> Option<(f64, f64)> {
    let g = LAST_LOCAL_TRAY_VALUES.lock().ok()?;
    *g
}

fn record_successful_sync() {
    if let Ok(mut g) = LAST_SUCCESSFUL_SYNC_AT.lock() {
        *g = Some(chrono::Utc::now());
    }
}

fn last_successful_sync_seconds_ago() -> Option<u64> {
    let g = LAST_SUCCESSFUL_SYNC_AT.lock().ok()?;
    let ts = (*g)?;
    let elapsed = chrono::Utc::now().signed_duration_since(ts);
    let secs = elapsed.num_seconds();
    if secs < 0 {
        // Clock skew — shouldn't happen, but if it does treat as
        // "just synced" rather than panicking.
        Some(0)
    } else {
        Some(secs as u64)
    }
}

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

/// v0.11.0 — headless launch-smoke marker. The frontend calls this once
/// on mount; in production (no `CLI_PULSE_SMOKE_MARKER` env) it is a pure
/// no-op. The CI launch-smoke job sets the env and polls for the file to
/// prove the app launched AND the React tree actually mounted. See
/// `smoke` module docs.
#[tauri::command]
fn smoke_mark_frontend_ready() -> Result<(), String> {
    match smoke::write_ready_marker() {
        Ok(true) => log::info!("launch-smoke: frontend-ready marker written"),
        Ok(false) => {} // production no-op — env var absent
        Err(e) => log::warn!("launch-smoke: failed to write marker: {e}"),
    }
    Ok(())
}

/// v0.11.x — is the launch-smoke env active? The frontend uses this to run a
/// one-shot tab-traversal render pass in smoke mode before it writes the
/// ready marker, so a per-tab render crash (v0.2.11 class) fails CI instead
/// of only a blank mount. Always `false` in production.
#[tauri::command]
fn smoke_is_active() -> bool {
    smoke::is_smoke_active()
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

/// System Monitor "Machine" tab — whole-machine CPU/mem + top-N process
/// table. LOCAL only (nothing synced). `spawn_blocking` because the sysinfo
/// two-sample refresh sleeps ~250ms.
#[tauri::command]
async fn get_machine_snapshot() -> Result<machine::MachineSnapshot, String> {
    async_runtime::spawn_blocking(machine::collect_machine_snapshot)
        .await
        .map_err(|e| format!("machine snapshot join error: {e}"))
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

/// v0.9.3 — Save a diagnostic bundle (zip) to ~/Downloads/. Includes
/// cli-pulse.log, remote-hook.log, crash-history.jsonl,
/// diagnostic_snapshot, and version info. Privacy posture: bundle
/// is saved LOCALLY; the user attaches it to a bug report
/// deliberately. Sensitive credentials (helper_secret, refresh_token,
/// OAuth tokens, JWTs) are NEVER included.
#[tauri::command]
fn save_diagnostic_bundle(
    app: tauri::AppHandle,
) -> Result<diagnostic_bundle::BundleResult, String> {
    // In-memory extras: the diagnostic_snapshot output and a
    // versions.txt sanity file. Both are lifecycle-fixed strings
    // that don't already live on disk.
    let snapshot = diagnostic_snapshot(app)?;
    let snapshot_json = serde_json::to_string_pretty(&snapshot).map_err(|e| e.to_string())?;
    let versions = format!(
        "tauri.conf.json version: {}\nCargo.toml version: {}\n",
        HELPER_VERSION, HELPER_VERSION,
    );
    let extras = vec![
        (
            "diagnostic_snapshot.json".to_string(),
            snapshot_json.into_bytes(),
        ),
        ("versions.txt".to_string(), versions.into_bytes()),
    ];
    diagnostic_bundle::create_bundle(extras).map_err(|e| e.to_string())
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

/// v0.5.0 — shared cache-aware fetch for `daily_usage`. Both
/// `get_daily_usage` (the Tauri command, called by Overview's
/// trend chart) and `get_cost_forecast` (the v0.5.0 forecast
/// Tauri command) share this path. Per Gemini 3.1 Pro v0.5.0
/// review: extracting avoids a small race window where two
/// Tauri commands firing simultaneously both miss cache and
/// fetch in parallel. With the helper, the second caller still
/// races but reads its own value into cache idempotently.
async fn ensure_daily_usage(
    user_id: &str,
    days: u32,
) -> Result<Vec<supabase::DailyUsageRow>, String> {
    if let Some(cached) = cache_get_daily_usage(user_id) {
        return Ok(cached);
    }
    let v = with_user_jwt(|jwt| async move { supabase::get_daily_usage(days, &jwt).await }).await?;
    cache_put_daily_usage(user_id, v.clone());
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
    ensure_daily_usage(&cfg.user_id, days).await
}

/// v0.5.3 — server-stored alerts for the RiskSignalsCard. Reads
/// the same `alerts` table that `dashboard_summary.unresolved_alerts`
/// counts, so the Overview tile and the Risk card are now sourced
/// from the same backend dataset. Previously the card used
/// `preview_alerts` (client-computed from local scan + thresholds)
/// which produced confusing divergence between the tile count and
/// the card content.
///
/// Returns `Ok(vec![])` for unpaired users — the card renders an
/// "(no paired account)" hint in that path. Network/auth errors
/// propagate as `Err` so the card can render a distinct
/// "Couldn't reach server" state (Gemini P2: defaulting an offline
/// fetch to the empty state would render "Looking good — no risk
/// signals" while the user is actually offline, which is a
/// dangerous false positive for budget alerts).
#[tauri::command]
async fn get_server_alerts() -> Result<Vec<supabase::ServerAlert>, String> {
    let Some(cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    let user_id = cfg.user_id.clone();
    with_user_jwt(move |jwt| {
        let user_id = user_id.clone();
        async move { supabase::get_unresolved_alerts(&user_id, &jwt).await }
    })
    .await
}

/// v0.10.1 — all alerts (open + resolved) for the Alerts tab's
/// Open/Resolved/All filter. Empty for unpaired users.
#[tauri::command]
async fn list_alerts() -> Result<Vec<supabase::ServerAlert>, String> {
    let Some(cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    let user_id = cfg.user_id.clone();
    with_user_jwt(move |jwt| {
        let user_id = user_id.clone();
        async move { supabase::list_alerts(&user_id, &jwt).await }
    })
    .await
}

/// v0.10.1 — alert lifecycle actions (macOS parity). Each PATCHes the
/// `alerts` row (RLS-scoped to the caller).
#[tauri::command]
async fn resolve_alert(id: String) -> Result<(), String> {
    with_user_jwt(move |jwt| {
        let id = id.clone();
        async move { supabase::resolve_alert(&id, &jwt).await }
    })
    .await
}

#[tauri::command]
async fn acknowledge_alert(id: String) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    with_user_jwt(move |jwt| {
        let id = id.clone();
        let now = now.clone();
        async move { supabase::acknowledge_alert(&id, &now, &jwt).await }
    })
    .await
}

#[tauri::command]
async fn snooze_alert(id: String, minutes: i64) -> Result<(), String> {
    let until = (chrono::Utc::now() + chrono::Duration::minutes(minutes)).to_rfc3339();
    with_user_jwt(move |jwt| {
        let id = id.clone();
        let until = until.clone();
        async move { supabase::snooze_alert(&id, &until, &jwt).await }
    })
    .await
}

/// v0.5.5 — Activity Timeline data source. Returns session rows from
/// the `sessions` table (cross-device historical view, RLS-scoped to
/// this user) for the last `hours` hours, capped at 1 000 rows.
///
/// Why not `list_sessions`: that command is a current-process snapshot
/// of THIS device's running CLI processes (sessions.rs:282-318),
/// truncated to 12 most-active. The Activity Timeline needs the
/// cross-device 24h history view, which lives only in the database.
/// Codex pre-implementation review caught this — the v1 plan would
/// have shipped a chart drawing the wrong dataset.
///
/// Returns `Ok(vec![])` for unpaired users (matches v0.5.2 / v0.5.3
/// convention — the chart hides on the no-paired state). Network /
/// auth errors propagate as `Err` so the chart can render a distinct
/// failure state.
#[tauri::command]
async fn get_sessions_history(
    hours: Option<u32>,
) -> Result<Vec<supabase::SessionHistoryRow>, String> {
    let hours = hours.unwrap_or(24).clamp(1, 168); // 1h..1w
    let Some(cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    let user_id = cfg.user_id.clone();
    let since = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
    with_user_jwt(move |jwt| {
        let user_id = user_id.clone();
        async move { supabase::get_sessions_history(&user_id, since, &jwt).await }
    })
    .await
}

/// v0.x — cross-device health read-back for the Machine tab's fleet section.
/// Returns the user's own devices + their last heartbeat-reported health
/// (CPU/mem/temp/battery/status). Empty when not paired / signed in.
#[tauri::command]
async fn get_devices() -> Result<Vec<supabase::DeviceHealthRow>, String> {
    let Some(cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    let user_id = cfg.user_id.clone();
    with_user_jwt(move |jwt| {
        let user_id = user_id.clone();
        async move { supabase::get_devices(&user_id, &jwt).await }
    })
    .await
}

/// v0.14 — provider service-status (public Atlassian Statuspage; no auth,
/// no pairing). Cached ~5min. Never errors — a failed fetch just omits that
/// provider.
#[tauri::command]
async fn get_service_statuses() -> Result<Vec<service_status::ServiceStatus>, String> {
    Ok(service_status::get_statuses().await)
}

/// v0.5.2 — top-projects aggregation. See `top_projects.rs` for
/// the algorithm; this command pulls the underlying `sessions`
/// rows for the past `days` days and returns the top-N rolled-up
/// projects sorted by total cost. Frontend `TopProjectsCard` on
/// the Overview tab renders the result.
///
/// Returns `Ok(vec![])` when the user isn't paired (same convention
/// as `get_cost_forecast`) — the card simply hides on the no-paired
/// state. Errors propagate to the frontend's per-card error state.
#[tauri::command]
async fn get_top_projects(days: Option<u32>) -> Result<Vec<top_projects::TopProject>, String> {
    let days = days.unwrap_or(30).clamp(1, 90);
    let Some(cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    let user_id = cfg.user_id.clone();
    let since = chrono::Utc::now() - chrono::Duration::days(days as i64);
    let rows = with_user_jwt(move |jwt| {
        let user_id = user_id.clone();
        async move { supabase::get_sessions_since(&user_id, since, &jwt).await }
    })
    .await?;
    Ok(top_projects::aggregate_top_projects(&rows, 5))
}

// ------------------------------------------------------------------------
// v0.6.0 — Remote Approvals (Slice 1: app-side view + decide).
//
// Wraps the existing live `remote_app_*` RPCs that the macOS team
// shipped on 2026-04-29 (Phase 1) and 2026-05-03 (Phase 2 iter1). The
// backend is fully present in Supabase; this slice adds the WINDOWS
// CLIENT for it. Hook emission, ConPTY managed-session host, and
// Send/Stop/Interrupt commands ship in later slices (v0.6.1, v0.7.0,
// v0.8.0 — see PROJECT_DEV_PLAN_2026-05-06_v0.6.0_remote_approvals_view.md).
//
// All 5 commands are unpaired-state-safe: return Ok(empty) / Ok(false)
// when `config::load()` returns None, matching the v0.5.2 / v0.5.3
// convention so the UI can distinguish "no data" from "actual error".
// ------------------------------------------------------------------------

#[tauri::command]
async fn get_remote_pending_approvals() -> Result<Vec<supabase::RemotePermissionRequest>, String> {
    let Some(_cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    with_user_jwt(|jwt| async move { supabase::remote_list_pending_approvals(&jwt).await }).await
}

/// Approve or deny a remote permission request.
///
/// **Defense-in-depth on high-risk approvals (Gemini 3.1 Pro v0.6.0
/// review P1):** before calling `remote_app_decide_permission` with
/// `decision="approve"`, re-fetch the live pending list and verify the
/// request's `risk` is NOT "high". This is layer 2 of 3 — layer 1 is
/// the helper-side risk classifier (which fail-closes before even
/// creating the row, so high-risk SHOULD never appear in the list);
/// layer 3 is the frontend disabling the Approve button. Both upstream
/// layers can fail (helper bypass, JS state desync). The Tauri command
/// is the last gate on a privacy-critical operation, so it pays for the
/// extra round-trip on Approve to ensure we never round-trip a
/// high-risk decision.
///
/// Deny is unconditional — refusing a request is always safe.
#[tauri::command]
async fn decide_remote_approval(
    request_id: String,
    decision: String,
    scope: String,
) -> Result<(), String> {
    let cfg = config::load()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Sign in required to decide remote approvals.".to_string())?;
    if decision != "approve" && decision != "deny" {
        return Err(format!(
            "Invalid decision `{decision}` — must be `approve` or `deny`."
        ));
    }
    if scope != "once" && scope != "alwaysSession" {
        return Err(format!(
            "Invalid scope `{scope}` — must be `once` or `alwaysSession`."
        ));
    }

    if decision == "approve" {
        // Re-fetch and verify risk ≠ "high" before approving. Adds one
        // RPC round-trip to the Approve path; Approve is user-initiated
        // and rare, so the cost is negligible vs. the security gain.
        let pending =
            with_user_jwt(|jwt| async move { supabase::remote_list_pending_approvals(&jwt).await })
                .await?;
        match pending.iter().find(|r| r.id == request_id) {
            Some(req) if req.risk == "high" => {
                return Err(
                    "High-risk requests can only be approved on the originating device — \
                     never from the desktop app."
                        .to_string(),
                );
            }
            None => {
                // Request no longer pending — likely decided on another
                // device, expired, or already approved. Return a typed
                // error string the frontend can match on to surface a
                // toast and refresh (Gemini 3.1 Pro v0.6.0 review Q6).
                return Err("ALREADY_DECIDED".to_string());
            }
            Some(_) => {} // proceed
        }
    }

    let device_id = cfg.device_id.clone();
    with_user_jwt(move |jwt| {
        let device_id = device_id.clone();
        let request_id = request_id.clone();
        let decision = decision.clone();
        let scope = scope.clone();
        async move {
            supabase::remote_decide_permission(
                &request_id,
                &decision,
                &scope,
                Some(&device_id),
                &jwt,
            )
            .await
        }
    })
    .await
}

#[tauri::command]
async fn list_remote_sessions() -> Result<Vec<supabase::RemoteSession>, String> {
    let Some(_cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    with_user_jwt(|jwt| async move { supabase::remote_list_sessions(&jwt).await }).await
}

/// v0.10.1 — Swarm View (macOS/iOS parity). Lists every agent swarm the
/// user's paired devices are heart-beating, via `remote_app_list_swarms`
/// (RC-gated server-side → `[]` when Remote Control is off). Returns an
/// empty list when this device isn't paired.
#[tauri::command]
async fn remote_list_swarms() -> Result<Vec<supabase::RemoteSwarmDevice>, String> {
    let Some(_cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    with_user_jwt(|jwt| async move { supabase::remote_list_swarms(&jwt).await }).await
}

/// v0.7.0 — One-click install of the CLI Pulse remote-approval hook
/// into Claude Code's `~/.claude/settings.json`. Wraps
/// `install_hook::install` with the currently-running binary's
/// absolute path.
///
/// Returns the structured result (Installed / AlreadyUpToDate /
/// Updated) so the frontend can render appropriate copy. Errors
/// surface as user-displayable strings (parse errors on existing
/// settings.json, write failures, etc.).
#[tauri::command]
fn install_claude_hook() -> Result<install_hook::InstallResult, String> {
    let bin = install_hook::current_binary_path();
    install_hook::install(&bin).map_err(|e| e.to_string())
}

/// v0.7.0 — Detect whether the hook is currently installed in
/// settings.json AND points to the running binary. Frontend uses
/// this to decide whether to render "Install" or "Installed" copy
/// in the Privacy section.
#[tauri::command]
fn get_claude_hook_status() -> Result<install_hook::HookStatus, String> {
    install_hook::current_status().ok_or_else(|| "could not resolve home directory".to_string())
}

/// v0.6.2 — send a command (prompt / stop / interrupt) to a managed
/// session running on any of the user's paired devices. Wraps the
/// existing live `remote_app_send_command` RPC.
///
/// `kind` validation duplicated here (also done server-side) so a
/// frontend bug doesn't ship a malformed kind. The 8192-char payload
/// cap is enforced server-side; we don't pre-trim so an over-cap
/// payload surfaces as a typed error the frontend can render
/// instead of a silent truncation.
#[tauri::command]
async fn send_remote_session_command(
    session_id: String,
    kind: String,
    payload: Option<String>,
) -> Result<(), String> {
    if kind != "prompt" && kind != "stop" && kind != "interrupt" {
        return Err(format!(
            "Invalid command kind `{kind}` — must be `prompt`, `stop`, or `interrupt`."
        ));
    }
    if kind == "prompt" {
        let trimmed = payload.as_deref().unwrap_or("").trim();
        if trimmed.is_empty() {
            return Err("Prompt command requires non-empty payload.".to_string());
        }
    }
    let _cfg = config::load()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Sign in required to send remote commands.".to_string())?;
    with_user_jwt(move |jwt| {
        let session_id = session_id.clone();
        let kind = kind.clone();
        let payload = payload.clone();
        async move {
            supabase::remote_send_command(&session_id, &kind, payload.as_deref(), &jwt).await
        }
    })
    .await
}

// ------------------------------------------------------------------------
// v0.9.1a — Spawn a managed Claude session on this device.
// ------------------------------------------------------------------------
//
// Frontend Sessions tab calls this when the user clicks "+ Start new
// session" → fills the dialog → submits. We:
//   1. Validate the user is signed in (need user JWT for the
//      `remote_app_request_session_start` RPC).
//   2. Compute cwd_hmac from the chosen path so the server can
//      coalesce "same project" across devices in a future iter.
//   3. Call `remote_app_request_session_start` with this device's own
//      device_id as target — the local agent loop's pull cycle picks
//      up the resulting `start` command.
//   4. In v0.9.1a the agent's `StubTransport.start()` returns
//      `Internal("not yet implemented")`, so the user sees "Spawn not
//      yet supported in this build" in the row's error UI. v0.9.1b
//      swaps the stub for ConPtyTransport and the spawn actually
//      succeeds.

#[derive(Debug, Clone, Deserialize)]
struct StartSessionArgs {
    cwd: String,
    #[serde(default)]
    cwd_basename: Option<String>,
    #[serde(default)]
    client_label: Option<String>,
    #[serde(default = "default_provider_args")]
    provider: String,
}

fn default_provider_args() -> String {
    "claude".to_string()
}

#[tauri::command]
async fn request_remote_session_start(args: StartSessionArgs) -> Result<String, String> {
    if args.provider != "claude" {
        return Err(format!(
            "Provider `{}` is not yet supported. v0.9.1a hosts Claude sessions only.",
            args.provider
        ));
    }
    let cfg = config::load()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Sign in required to start a remote session.".to_string())?;

    let basename = args.cwd_basename.clone().unwrap_or_else(|| {
        args.cwd
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .chars()
            .take(255)
            .collect::<String>()
    });
    let hmac_secret = cwd_hmac::load_or_create_secret().ok().flatten();
    let cwd_hmac_hex = hmac_secret
        .as_deref()
        .and_then(|s| cwd_hmac::hmac_path(s, &args.cwd));

    let device_id = cfg.device_id.clone();
    let provider = args.provider.clone();
    let client_label = args.client_label.clone();

    with_user_jwt(move |jwt| {
        let device_id = device_id.clone();
        let provider = provider.clone();
        let basename = basename.clone();
        let cwd_hmac_hex = cwd_hmac_hex.clone();
        let client_label = client_label.clone();
        async move {
            supabase::remote_request_session_start(
                &device_id,
                &provider,
                Some(&basename),
                cwd_hmac_hex.as_deref(),
                client_label.as_deref(),
                &jwt,
            )
            .await
        }
    })
    .await
}

/// v0.9.1a — agent loop diagnostic for Settings → About display.
/// Returns null when the agent isn't running (not paired, in recovery
/// mode, or kill-switched via env var).
#[tauri::command]
fn agent_diagnostic(app: tauri::AppHandle) -> Option<remote::AgentDiagnostic> {
    use tauri::Manager;
    let state = app.try_state::<RemoteAgentState>()?;
    Some(state.handle.manager().diagnostic())
}

/// State wrapper held by Tauri. Stashes the `AgentHandle` so the
/// `agent_diagnostic` command can read live counters and so the
/// shutdown path can stop the loop cleanly.
struct RemoteAgentState {
    handle: remote::AgentHandle,
}

// ------------------------------------------------------------------------
// v0.8.1 — `request_remote_session_start` and `agent_diagnostic` Tauri
// commands shipped in v0.8.0 are removed in this revert; they were the
// frontend entry points to the ConPTY managed-session host that
// crashed on launch on Windows. The corresponding code is gone in
// v0.8.1; ConPTY work resumes on the v0.9.x track with a mandatory
// VM smoke gate before promote-to-Latest.
// ------------------------------------------------------------------------

#[tauri::command]
async fn get_remote_control_setting() -> Result<bool, String> {
    let Some(cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    let user_id = cfg.user_id.clone();
    with_user_jwt(move |jwt| {
        let user_id = user_id.clone();
        async move { supabase::get_remote_control_setting(&user_id, &jwt).await }
    })
    .await
}

/// Toggle the user's `remote_control_enabled` server-side setting.
///
/// Per Gemini 3.1 Pro v0.6.0 review P0: caller MUST revert any
/// optimistic UI when this returns Err. Showing "Remote Control: ON"
/// while the server holds "OFF" misleads the user about their privacy
/// posture — a feature whose purpose IS privacy must never lie about
/// its state.
#[tauri::command]
async fn set_remote_control_setting(enabled: bool) -> Result<(), String> {
    let cfg = config::load()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Sign in required to change Remote Control.".to_string())?;
    let user_id = cfg.user_id.clone();
    with_user_jwt(move |jwt| {
        let user_id = user_id.clone();
        async move { supabase::set_remote_control_setting(&user_id, enabled, &jwt).await }
    })
    .await
}

// ------------------------------------------------------------------------
// v0.5.4 — Settings → Danger Zone. Two destructive actions:
//   - clear_local_caches: wipes in-memory dashboard cache + on-disk scan
//     cache (cost-usage). User stays signed in; next sync re-fetches
//     everything. Reversible.
//   - delete_account_and_unpair: server-side `delete_user_account` RPC
//     followed by best-effort local clear. Permanent.
//
// CRITICAL ORDERING for delete (Codex P1 + Gemini 3.1 Pro P1, both flagged
// independently): RPC FIRST, then local clear. `with_user_jwt` reads the
// refresh_token from keychain (lib.rs:688) to mint the JWT; clearing the
// keychain BEFORE the RPC would ship an unauthenticated request and
// silently leave the server row intact while the user thought their data
// was gone — Gemini called this "a massive trust/privacy violation."
// ------------------------------------------------------------------------

/// Clear all in-memory and on-disk local caches. Does NOT touch keychain
/// or config — user stays signed in; the next background sync (or manual
/// "Sync now") re-fetches everything from the server. Reversible.
///
/// Scope (per Codex P2 reviewer finding — be explicit, not aspirational):
///   - in-memory `DASHBOARD_CACHE` (dashboard_summary / provider_summary
///     / daily_usage rows, 30 s TTL — see lib.rs:573)
///   - on-disk `cache::wipe_all(None)` (scan cache under
///     `<cache_dir>/cost-usage/{provider}-v1.json` — Codex / Claude
///     incremental scan caches)
///
/// Provider creds and refresh tokens are explicitly NOT wiped here —
/// that's `delete_account_and_unpair`'s scope. The Danger Zone copy
/// makes this distinction explicit so users know "Clear caches" leaves
/// them signed in.
#[tauri::command]
fn clear_local_caches() -> Result<(), String> {
    cache_invalidate();
    if let Err(e) = cache::wipe_all(None) {
        // Disk cache wipe failure is rare (permissions / antivirus
        // lock) but recoverable: we report it to the frontend so the
        // user knows what went wrong, but in-mem cache was already
        // cleared so the immediate UI state is fresh anyway.
        return Err(format!("Failed to wipe scan cache: {e}"));
    }
    Ok(())
}

/// Permanently delete the user's cloud account and unpair this device.
///
/// Steps (ordering matters — see module-level comment):
///   1. Mint a fresh user JWT via `with_user_jwt` (reads refresh_token
///      from keychain → refreshes → persists rotated → returns access_token).
///   2. Call `supabase::delete_user_account(jwt)` RPC. Server cascades
///      the delete through `auth.users` to all owned rows.
///   3. ON SUCCESS ONLY: best-effort local clear:
///      - `cache_invalidate()` + `cache::wipe_all(None)`
///      - `keychain::delete_refresh_token()`
///      - `provider_creds::wipe()` (all 4 cred slots + the file fallback)
///      - `config::clear()`
///
/// On RPC error: return `Err` to the frontend, leave local state
/// intact. The user can retry — server-side data still exists, and
/// keeping the local refresh_token / config means they don't have to
/// re-OTP just to retry the delete.
///
/// Best-effort means each local clear step logs::warn on failure but
/// does not abort the others — by the time we're here, the server row
/// is already gone, so partial local cleanup is strictly better than
/// rolling back.
#[tauri::command]
async fn delete_account_and_unpair() -> Result<(), String> {
    // Step 1+2: RPC FIRST. with_user_jwt handles the keychain read →
    // refresh → persist-rotated → call sequence atomically (lib.rs:688).
    // If the user's session is expired, with_user_jwt returns the
    // "Session expired — sign in again" error and clears keychain itself
    // — that's the right behavior (no usable JWT means we can't auth
    // the delete; the user must re-OTP first).
    with_user_jwt(|jwt| async move { supabase::delete_user_account(&jwt).await }).await?;

    // Step 3: best-effort local clear. By this point the server row is
    // gone; we want as much local cleanup as possible but partial
    // failure here doesn't undo the server delete.
    cache_invalidate();
    if let Err(e) = cache::wipe_all(None) {
        log::warn!("delete_account: scan cache wipe failed (non-fatal): {e}");
    }
    if let Err(e) = keychain::delete_refresh_token() {
        log::warn!("delete_account: refresh_token delete failed (non-fatal): {e:?}");
    }
    if let Err(e) = provider_creds::wipe() {
        log::warn!("delete_account: provider_creds wipe failed (non-fatal): {e}");
    }
    if let Err(e) = config::clear() {
        // config::clear failure leaves the device file behind; the next
        // app launch will read a stale `paired=true` config but the
        // server row is gone, so the next background_tick will fail with
        // 401/404 and `classify_auth_failure` will detect device_missing
        // / account_missing and clean up. So this is recoverable, just
        // not immediate. Surface the warning via log; do NOT bubble up
        // because that would suggest the delete itself failed.
        log::warn!("delete_account: config clear failed (will self-heal on next sync): {e}");
    }
    Ok(())
}

/// v0.5.0 — month-end cost forecast (Mac sibling parity port of
/// `CostForecastEngine.swift`). Reads the same `daily_usage` rows
/// that power Overview's trend chart, runs linear regression on
/// per-day cost summed across providers/models, and returns a
/// predicted month-end total with 1-stddev bounds.
///
/// Returns `Ok(None)` only when the user isn't paired (no JWT, no
/// daily-usage data to forecast from). When paired but with zero
/// usage, returns `Ok(Some(forecast))` with `is_reliable: false` and
/// zero values — UI shows an "not enough data yet" hint rather than
/// a blank card.
#[tauri::command]
async fn get_cost_forecast() -> Result<Option<cost_forecast::CostForecast>, String> {
    let Some(cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let daily = ensure_daily_usage(&cfg.user_id, 30).await?;
    let today = chrono::Local::now().date_naive();
    Ok(cost_forecast::forecast_from_daily(&daily, today))
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
    deepseek_api_key_set: bool,
    zai_api_key_set: bool,
    crof_api_key_set: bool,
    minimax_api_key_set: bool,
    moonshot_api_key_set: bool,
    venice_api_key_set: bool,
    kimi_k2_api_key_set: bool,
    augment_cookie_set: bool,
    perplexity_cookie_set: bool,
    t3chat_cookie_set: bool,
    stepfun_cookie_set: bool,
    warp_api_key_set: bool,
    kimi_auth_token_set: bool,
    grok_cookie_set: bool,
    glm_api_key_set: bool,
    volcano_api_key_set: bool,
    groq_api_key_set: bool,
    mistral_cookie_set: bool,
    deepgram_api_key_set: bool,
    elevenlabs_api_key_set: bool,
    kilo_api_key_set: bool,
    alibaba_api_key_set: bool,
    openai_admin_key_set: bool,
    codebuff_api_key_set: bool,
    manus_cookie_set: bool,
    abacus_cookie_set: bool,
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
    env_override_deepseek: bool,
    env_override_zai: bool,
    env_override_crof: bool,
    env_override_minimax: bool,
    env_override_moonshot: bool,
    env_override_venice: bool,
    env_override_kimi_k2: bool,
    env_override_augment: bool,
    env_override_perplexity: bool,
    env_override_t3chat: bool,
    env_override_stepfun: bool,
    env_override_warp: bool,
    env_override_kimi: bool,
    env_override_grok: bool,
    env_override_glm: bool,
    env_override_volcano: bool,
    env_override_groq: bool,
    env_override_mistral: bool,
    env_override_deepgram: bool,
    env_override_elevenlabs: bool,
    env_override_kilo: bool,
    env_override_alibaba: bool,
    env_override_openai_admin: bool,
    env_override_codebuff: bool,
    env_override_manus: bool,
    env_override_abacus: bool,
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
    deepseek_api_key: Option<String>,
    zai_api_key: Option<String>,
    crof_api_key: Option<String>,
    minimax_api_key: Option<String>,
    moonshot_api_key: Option<String>,
    venice_api_key: Option<String>,
    kimi_k2_api_key: Option<String>,
    augment_cookie: Option<String>,
    perplexity_cookie: Option<String>,
    t3chat_cookie: Option<String>,
    stepfun_cookie: Option<String>,
    warp_api_key: Option<String>,
    kimi_auth_token: Option<String>,
    grok_cookie: Option<String>,
    glm_api_key: Option<String>,
    volcano_api_key: Option<String>,
    groq_api_key: Option<String>,
    mistral_cookie: Option<String>,
    deepgram_api_key: Option<String>,
    elevenlabs_api_key: Option<String>,
    kilo_api_key: Option<String>,
    alibaba_api_key: Option<String>,
    openai_admin_key: Option<String>,
    codebuff_api_key: Option<String>,
    manus_cookie: Option<String>,
    abacus_cookie: Option<String>,
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
        deepseek_api_key_set: c.deepseek_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        zai_api_key_set: c.zai_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        crof_api_key_set: c.crof_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        minimax_api_key_set: c.minimax_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        moonshot_api_key_set: c.moonshot_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        venice_api_key_set: c.venice_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        kimi_k2_api_key_set: c.kimi_k2_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        augment_cookie_set: c.augment_cookie.as_deref().is_some_and(|s| !s.is_empty()),
        perplexity_cookie_set: c
            .perplexity_cookie
            .as_deref()
            .is_some_and(|s| !s.is_empty()),
        t3chat_cookie_set: c.t3chat_cookie.as_deref().is_some_and(|s| !s.is_empty()),
        stepfun_cookie_set: c.stepfun_cookie.as_deref().is_some_and(|s| !s.is_empty()),
        warp_api_key_set: c.warp_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        kimi_auth_token_set: c.kimi_auth_token.as_deref().is_some_and(|s| !s.is_empty()),
        grok_cookie_set: c.grok_cookie.as_deref().is_some_and(|s| !s.is_empty()),
        glm_api_key_set: c.glm_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        volcano_api_key_set: c.volcano_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        groq_api_key_set: c.groq_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        mistral_cookie_set: c.mistral_cookie.as_deref().is_some_and(|s| !s.is_empty()),
        deepgram_api_key_set: c.deepgram_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        elevenlabs_api_key_set: c
            .elevenlabs_api_key
            .as_deref()
            .is_some_and(|s| !s.is_empty()),
        kilo_api_key_set: c.kilo_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        alibaba_api_key_set: c.alibaba_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        openai_admin_key_set: c.openai_admin_key.as_deref().is_some_and(|s| !s.is_empty()),
        codebuff_api_key_set: c.codebuff_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        manus_cookie_set: c.manus_cookie.as_deref().is_some_and(|s| !s.is_empty()),
        abacus_cookie_set: c.abacus_cookie.as_deref().is_some_and(|s| !s.is_empty()),
        openrouter_base_url: c.openrouter_base_url.clone(),
        env_override_cursor: env_set("CURSOR_COOKIE"),
        env_override_copilot: env_set("COPILOT_API_TOKEN"),
        env_override_openrouter_key: env_set("OPENROUTER_API_KEY"),
        env_override_openrouter_url: env_set("OPENROUTER_API_URL"),
        env_override_deepseek: env_set("DEEPSEEK_API_KEY") || env_set("DEEPSEEK_KEY"),
        env_override_zai: env_set("Z_AI_API_KEY"),
        env_override_crof: env_set("CROF_API_KEY"),
        env_override_minimax: env_set("MINIMAX_API_KEY"),
        env_override_moonshot: env_set("MOONSHOT_API_KEY"),
        env_override_venice: env_set("VENICE_API_KEY") || env_set("VENICE_KEY"),
        env_override_kimi_k2: env_set("KIMI_K2_API_KEY")
            || env_set("KIMI_API_KEY")
            || env_set("KIMI_KEY"),
        env_override_augment: env_set("AUGMENT_COOKIE"),
        env_override_perplexity: env_set("PERPLEXITY_SESSION_TOKEN")
            || env_set("PERPLEXITY_COOKIE"),
        env_override_t3chat: env_set("T3CHAT_COOKIE"),
        env_override_stepfun: env_set("STEPFUN_COOKIE") || env_set("STEPFUN_OASIS_TOKEN"),
        env_override_warp: env_set("WARP_API_KEY") || env_set("WARP_TOKEN"),
        env_override_kimi: env_set("KIMI_AUTH_TOKEN"),
        env_override_grok: env_set("GROK_COOKIE") || env_set("GROK_TOKEN"),
        env_override_glm: env_set("GLM_API_KEY")
            || env_set("ZHIPU_API_KEY")
            || env_set("CHATGLM_API_KEY"),
        env_override_volcano: env_set("ARK_API_KEY")
            || env_set("VOLC_ACCESSKEY")
            || env_set("VOLCANO_ENGINE_API_KEY"),
        env_override_groq: env_set("GROQ_API_KEY"),
        env_override_mistral: env_set("MISTRAL_COOKIE") || env_set("MISTRAL_SESSION_TOKEN"),
        env_override_deepgram: env_set("DEEPGRAM_API_KEY"),
        env_override_elevenlabs: env_set("ELEVENLABS_API_KEY") || env_set("XI_API_KEY"),
        env_override_kilo: env_set("KILO_API_KEY"),
        env_override_alibaba: env_set("ALIBABA_CODING_PLAN_API_KEY"),
        env_override_openai_admin: env_set("OPENAI_ADMIN_KEY"),
        env_override_codebuff: env_set("CODEBUFF_API_KEY"),
        env_override_manus: env_set("MANUS_SESSION_TOKEN")
            || env_set("MANUS_SESSION_ID")
            || env_set("MANUS_COOKIE"),
        env_override_abacus: env_set("ABACUS_COOKIE") || env_set("ABACUS_SESSION_TOKEN"),
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
    if let Some(v) = update.deepseek_api_key {
        current.deepseek_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.zai_api_key {
        current.zai_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.crof_api_key {
        current.crof_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.minimax_api_key {
        current.minimax_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.moonshot_api_key {
        current.moonshot_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.venice_api_key {
        current.venice_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.kimi_k2_api_key {
        current.kimi_k2_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.augment_cookie {
        current.augment_cookie = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.perplexity_cookie {
        current.perplexity_cookie = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.t3chat_cookie {
        current.t3chat_cookie = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.stepfun_cookie {
        current.stepfun_cookie = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.warp_api_key {
        current.warp_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.kimi_auth_token {
        current.kimi_auth_token = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.grok_cookie {
        current.grok_cookie = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.glm_api_key {
        current.glm_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.volcano_api_key {
        current.volcano_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.groq_api_key {
        current.groq_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.mistral_cookie {
        current.mistral_cookie = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.deepgram_api_key {
        current.deepgram_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.elevenlabs_api_key {
        current.elevenlabs_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.kilo_api_key {
        current.kilo_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.alibaba_api_key {
        current.alibaba_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.openai_admin_key {
        current.openai_admin_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.codebuff_api_key {
        current.codebuff_api_key = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.manus_cookie {
        current.manus_cookie = if v.is_empty() { None } else { Some(v) };
    }
    if let Some(v) = update.abacus_cookie {
        current.abacus_cookie = if v.is_empty() { None } else { Some(v) };
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
    /// LOCAL-ONLY human-readable status line from the snapshot (e.g.
    /// `"$12.34 balance"`). Display-only — never uploaded. Rendered as the
    /// provider card's secondary line for balance/status-only providers.
    #[serde(skip_serializing_if = "Option::is_none")]
    status_text: Option<String>,
}

#[tauri::command]
fn get_last_collector_status() -> Vec<CollectorStatusView> {
    quota::last_outcomes()
        .into_iter()
        .map(|o| CollectorStatusView {
            provider: o.provider,
            ok: o.snapshot.is_some(),
            status_text: o.snapshot.and_then(|s| s.status_text),
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

    // v0.5.6 — record success timestamp here, AFTER all the sync
    // sub-steps (helper_sync + helper_sync_daily_usage) returned ok.
    // Updating from the sub-step success branches would mark the
    // whole sync "successful" even when one sub-step silently
    // errored.
    record_successful_sync();

    // v0.5.7 — stash local-scan-derived (month_so_far, forecast) for
    // the tray menu. See `LAST_LOCAL_TRAY_VALUES` doc comment for the
    // VM-verify-2026-05-06 P2 finding this addresses. Computation is
    // ~O(scan.entries.len()) + the forecast linear regression; both
    // are negligible relative to the sync we just finished. Wrapped
    // in `record_local_tray_snapshot` to keep `perform_sync`'s body
    // focused on the sync orchestration.
    record_local_tray_snapshot(&scan);

    // v0.17.0 + v0.18.1 — cross-device heartbeat. Report whole-device
    // CPU%/mem% + active-session count (+ capability-gated temps/battery in
    // p_metrics) to the `devices` row so the user's OTHER devices (phone /
    // Mac) can show this machine's health. Best-effort and last: a heartbeat
    // failure must NOT fail the sync (sessions / alerts / usage already
    // landed). Rides the same 120s tick. `helper_sync` already marks the
    // device Online; heartbeat adds cpu / mem / session-count — a benign
    // double status-write (both set `now()`). `provider_plan_status` is None
    // (the desktop isn't a managed on-plan host yet); `p_metrics` carries the
    // sensor blob when readable, else None → the server's per-field coalesce
    // preserves last-known values rather than clobbering them.
    let (cpu_pct, mem_pct) = async_runtime::spawn_blocking(machine::collect_load)
        .await
        .unwrap_or((0, 0));
    let sensor_metrics = async_runtime::spawn_blocking(machine::collect_sensor_metrics)
        .await
        .unwrap_or(None);
    if let Err(e) = supabase::helper_heartbeat(&supabase::HelperHeartbeatRequest {
        p_device_id: &cfg.device_id,
        p_helper_secret: &cfg.helper_secret,
        p_cpu_usage: cpu_pct,
        p_memory_usage: mem_pct,
        p_active_session_count: snapshot.sessions.len() as i32,
        p_provider_plan_status: None,
        p_metrics: sensor_metrics,
    })
    .await
    {
        log::warn!("helper_heartbeat failed (non-fatal): {}", friendly(e));
    }

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
// v0.5.6 — Tray mini-metrics refresh.
//
// Builds `TrayMetrics` from the existing 30 s-TTL `DASHBOARD_CACHE`
// (lib.rs:573) plus the in-process `LAST_SUCCESSFUL_SYNC_AT` global.
// Per Codex pre-implementation review: the tray reads cached values
// only — never triggers a fresh network fetch. Cache miss == render
// `None` for that field; the next user-driven UI fetch will populate
// the cache and the next tray tick (or force-refresh) picks it up.
// This sidesteps the v0.5.6 P2 cache-race concern entirely: if a
// real user fetch is mid-flight, the tray waits for the next tick
// rather than racing for the same data.
// ------------------------------------------------------------------------

fn collect_tray_metrics() -> tray::TrayMetrics {
    let cfg = match config::load() {
        Ok(Some(cfg)) => cfg,
        _ => {
            return tray::TrayMetrics {
                paired: false,
                ..Default::default()
            };
        }
    };

    // v0.5.7 hotfix — primary data source is the local-scan-derived
    // snapshot stashed by `record_local_tray_snapshot` at the end of
    // every successful `perform_sync` (T+~20 s after launch, then
    // every 120 s). Always fresh while the device is paired; never
    // depends on which tab the user has open. VM verify 2026-05-06
    // caught the v0.5.6 bug where the tray read DASHBOARD_CACHE
    // (only populated by Overview's CostForecastCard polling) and
    // therefore showed "—" forever for users who minimized to tray.
    //
    // Fall back to DASHBOARD_CACHE.daily_usage on the brand-new
    // launch path before any sync has run (perform_sync hasn't
    // populated LAST_LOCAL_TRAY_VALUES yet, but Overview might have
    // been opened and populated the dashboard cache). This window is
    // narrow and the fallback rarely fires in practice.
    let (month_so_far, forecast_total) = match last_local_tray_values() {
        Some((month, forecast)) => (Some(month), Some(forecast)),
        None => {
            // Pre-first-sync fallback. Per Codex P2: still NEVER mint
            // a fresh JWT from the tray path — read the cache only.
            let daily = cache_get_daily_usage(&cfg.user_id);
            let today = chrono::Local::now().date_naive();
            match daily.as_deref() {
                Some(rows) => match cost_forecast::forecast_from_daily(rows, today) {
                    Some(f) => (Some(f.actual_to_date), Some(f.predicted_month_total)),
                    None => (None, None),
                },
                None => (None, None),
            }
        }
    };

    tray::TrayMetrics {
        month_so_far_usd: month_so_far,
        forecast_usd: forecast_total,
        synced_seconds_ago: last_successful_sync_seconds_ago(),
        paired: true,
    }
}

/// Tauri command — re-render the tray menu now, optionally with new
/// localized copy. The frontend's language-change handler invokes
/// this with the freshly-translated strings so the tray flips
/// immediately instead of waiting up to 120 s for the next refresh
/// loop tick (Gemini 3.1 Pro pre-implementation P2 on language
/// desync).
///
/// Calling without `copy` (e.g. from a manual "tray-refresh-clicked"
/// handler) just re-applies metrics with the currently-stored copy.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrayCopyPayload {
    header_label: String,
    month_so_far_template: String,
    forecast_template: String,
    synced_ago_template: String,
    synced_never: String,
    not_paired: String,
    no_data: String,
    open_label: String,
    quit_label: String,
}

impl From<TrayCopyPayload> for tray::TrayCopy {
    fn from(p: TrayCopyPayload) -> Self {
        Self {
            header_label: p.header_label,
            month_so_far_template: p.month_so_far_template,
            forecast_template: p.forecast_template,
            synced_ago_template: p.synced_ago_template,
            synced_never: p.synced_never,
            not_paired: p.not_paired,
            no_data: p.no_data,
            open_label: p.open_label,
            quit_label: p.quit_label,
        }
    }
}

#[tauri::command]
fn force_tray_menu_refresh(
    app: tauri::AppHandle,
    copy: Option<TrayCopyPayload>,
) -> Result<(), String> {
    if let Some(c) = copy {
        tray::set_copy(&app, c.into());
    }
    tray::apply_metrics(&app, &collect_tray_metrics());
    Ok(())
}

/// 120 s loop that re-renders the tray menu's three dynamic rows
/// from cached data. Stop-responsive — both the 30 s warm-up AND
/// every 120 s tick race their sleep against `poll_stop_signal`,
/// so a shutdown raised mid-sleep is observed within ~100 ms
/// (matches the v0.4.23 `wait_for_next_tick` invariant for the
/// main sync loop). Per Gemini 3.1 Pro v0.5.6 P1: the original
/// `interval.tick().await` would block up to 120 s without
/// observing `stop`, delaying clean app shutdown by up to 2 min.
fn spawn_tray_refresh_loop(app: tauri::AppHandle, stop: Arc<AtomicBool>) {
    async_runtime::spawn(async move {
        log::info!(
            "Tray refresh loop started — first tick in 30s, then every {}s",
            TRAY_REFRESH_INTERVAL.as_secs()
        );
        // First tick after 30 s — long enough for the initial
        // `background_tick` (T+20s) to populate the cache so the
        // first tray render shows real values, not placeholders.
        // Race the sleep against `poll_stop_signal` so a shutdown
        // during the warm-up exits within ~100 ms instead of
        // waiting out the full 30 s.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            _ = poll_stop_signal(&stop) => return,
        }

        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            tray::apply_metrics(&app, &collect_tray_metrics());
            // Stop-responsive sleep at the tick cadence. Bare
            // `tokio::time::interval(...).tick().await` would block
            // the full 120 s without polling `stop` — race against
            // the stop poller for the same ~100 ms shutdown
            // latency the main sync loop guarantees.
            tokio::select! {
                _ = tokio::time::sleep(TRAY_REFRESH_INTERVAL) => {}
                _ = poll_stop_signal(&stop) => return,
            }
        }
    });
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

    // v0.9.0 — record this launch in crash-history.jsonl FIRST,
    // before anything risky runs. If we crash before
    // record_setup_complete() at the end of the setup hook, the next
    // launch's `assess_recovery_mode_at_startup()` sees this orphan
    // `Starting` entry as evidence of a crash. (See `crash_recovery`
    // module docstring for the full rationale.)
    crash_recovery::record_startup();
    let recovery_active = crash_recovery::assess_recovery_mode_at_startup();

    // Sentry — no-op when CLI_PULSE_SENTRY_DSN is unset (the default).
    // Install before tauri::Builder so the panic handler is registered
    // for the lifetime of the process.
    //
    // v0.9.0 (Gemini plan-review P2): keep Sentry ON even in recovery
    // mode. The whole point of crash recovery is to debug the crash
    // loop; turning off telemetry would blind us at exactly the wrong
    // time.
    sentry_init::install();

    if recovery_active {
        log::warn!("Recovery mode active — agent loop and tray refresh will be skipped");
    }

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
        //
        // v0.8.2 Sentry-driven fix: the Stdout target was the source of
        // DESKTOP-2/DESKTOP-3 panics ("Error performing stderr logging
        // after error occurred during regular logging" → "The pipe is
        // being closed (os error 232)"). Windows GUI release builds
        // have no console attached; stdout is a closed pipe; writes
        // fail; tauri-plugin-log falls back to stderr, which also
        // fails; the underlying `log` crate panics. Pre-existed v0.7.0
        // (multiple Sentry events 2026-05-07/08); not caused by v0.8.0
        // ConPTY incident, just surfaced by it.
        // Fix: only attach Stdout target in debug builds (cargo run /
        // cargo tauri dev). Release builds rely on the LogDir target
        // — which is also where bug-report copy-paste reads from.
        .plugin(
            tauri_plugin_log::Builder::default()
                .level(log::LevelFilter::Info)
                .targets({
                    let mut targets = vec![tauri_plugin_log::Target::new(
                        tauri_plugin_log::TargetKind::LogDir {
                            file_name: Some("cli-pulse".into()),
                        },
                    )];
                    #[cfg(debug_assertions)]
                    targets.push(tauri_plugin_log::Target::new(
                        tauri_plugin_log::TargetKind::Stdout,
                    ));
                    targets
                })
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

            // v0.9.1a — ConPTY managed-session agent loop returns
            // (scaffolding only; v0.9.1b adds the actual FFI). The
            // v0.8.0 root cause was using `tokio::spawn` from this
            // setup hook; we now use `tauri::async_runtime::spawn`
            // inside `remote::spawn_agent_loop` which has the right
            // runtime context.
            //
            // Three-way gate before spawning:
            //  1. Device is paired (need helper_secret for RPC auth)
            //  2. NOT in recovery mode (v0.9.0 crash-loop circuit
            //     breaker — if we're here because of agent-related
            //     crashes, don't re-spawn the agent)
            //  3. CLI_PULSE_DISABLE_REMOTE_AGENT env var unset
            //     (kill-switch for users who hit a future bug)
            match config::load() {
                Ok(Some(cfg_for_agent)) => {
                    let kill_switch = std::env::var("CLI_PULSE_DISABLE_REMOTE_AGENT").is_ok();
                    if crash_recovery::is_in_recovery_mode() {
                        log::warn!("Remote agent loop NOT spawned — recovery mode active");
                    } else if kill_switch {
                        log::warn!(
                            "Remote agent loop NOT spawned — \
                             CLI_PULSE_DISABLE_REMOTE_AGENT env var set"
                        );
                    } else {
                        let transport: std::sync::Arc<dyn remote::SessionTransport> =
                            std::sync::Arc::new(remote::ConPtyTransport::new());
                        let agent_handle =
                            remote::spawn_agent_loop(transport, cfg_for_agent, stop_bg.clone());
                        app.manage(RemoteAgentState {
                            handle: agent_handle,
                        });
                        log::info!("Remote agent loop started (ConPtyTransport — v0.9.2)");
                    }
                }
                Ok(None) => log::info!(
                    "Remote agent loop not started — device not paired \
                     (sign in via Settings to enable)"
                ),
                Err(e) => log::warn!("Remote agent loop not started — config load failed: {e}"),
            }

            // System tray — Windows first-class, Linux works with AppIndicator
            // when libayatana-appindicator3 is installed, otherwise we log and
            // continue window-first.
            //
            // v0.5.6 — tray now shows live mini-metrics (month so far +
            // forecast + synced-ago). We start the refresh loop only on
            // successful tray install: a Linux user without
            // libayatana-appindicator3 sees no tray at all (per the
            // module-level comment), and there's nothing for the loop
            // to mutate.
            //
            // v0.9.0 — skip the tray refresh loop in recovery mode.
            // Tray ITSELF still installs (so users can Quit via the
            // tray icon and aren't stuck), but the 120 s refresh loop
            // — which runs cached metric reads — is one of the
            // potential failure surfaces we cut off until the user
            // re-enables.
            if !crash_recovery::is_in_recovery_mode() {
                match tray::install(app.handle()) {
                    Ok(()) => {
                        spawn_tray_refresh_loop(app.handle().clone(), stop_bg.clone());
                    }
                    Err(e) => {
                        log::warn!("tray init failed (continuing without tray): {e}");
                    }
                }
            } else {
                // Even in recovery mode, install the tray itself so
                // users can Quit cleanly. Skip only the refresh loop.
                if let Err(e) = tray::install(app.handle()) {
                    log::warn!("tray init failed in recovery mode (continuing): {e}");
                }
                log::warn!(
                    "Recovery mode: tray refresh loop skipped (re-enable in Settings → About)"
                );
            }

            // v0.9.0 — Tauri setup hook completed without panic.
            // Mark this so the next launch's
            // `assess_recovery_mode_at_startup()` sees this run as
            // healthy, not an incomplete startup.
            crash_recovery::record_setup_complete();

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            // v0.11.0 — headless launch-smoke frontend-ready marker
            smoke_mark_frontend_ready,
            smoke_is_active,
            scan_usage,
            list_sessions,
            // System Monitor "Machine" tab (local CPU/mem + top-N processes)
            get_machine_snapshot,
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
            // v0.5.0 — month-end cost forecast (Mac sibling parity)
            get_cost_forecast,
            // v0.5.2 — top-projects aggregation from sessions table
            get_top_projects,
            // v0.5.3 — server-stored unresolved alerts for RiskSignalsCard
            get_server_alerts,
            // v0.5.4 — Settings → Danger Zone (clear caches + delete account)
            clear_local_caches,
            delete_account_and_unpair,
            // v0.5.5 — Activity Timeline data source (sessions table 24h history)
            get_sessions_history,
            // Cross-device health read-back (Machine tab fleet section)
            get_devices,
            // v0.14 — provider service-status badges (public Statuspage)
            get_service_statuses,
            // v0.5.6 — Tray mini-metrics force-refresh (language change path)
            force_tray_menu_refresh,
            // v0.6.0 — Remote Approvals (app-side view + decide)
            get_remote_pending_approvals,
            decide_remote_approval,
            list_remote_sessions,
            remote_list_swarms,
            list_alerts,
            resolve_alert,
            acknowledge_alert,
            snooze_alert,
            get_remote_control_setting,
            set_remote_control_setting,
            // v0.6.2 — Send / Stop / Interrupt managed sessions
            send_remote_session_command,
            // v0.9.1a — ConPTY managed-session local host
            request_remote_session_start,
            agent_diagnostic,
            // v0.9.3 — Save diagnostic bundle (zip) to ~/Downloads/
            save_diagnostic_bundle,
            // v0.7.0 — Install Claude hook + check current status
            install_claude_hook,
            get_claude_hook_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    // Let the background loop exit cleanly on app shutdown.
    stop.store(true, Ordering::Relaxed);

    // v0.9.1a — give the remote agent loop a brief window to drain
    // its final tick + post `kind=info` `app_shutdown` for any
    // running sessions. Sleep 2s after stop so the loop has time
    // to react. The agent's own per-call timeouts (5s) bound this
    // path even if a hung Supabase couldn't block process exit
    // beyond a couple of seconds.
    std::thread::sleep(std::time::Duration::from_secs(2));

    // v0.9.0 — record clean exit so the next launch's
    // `assess_recovery_mode_at_startup()` sees this run as healthy.
    // (A crashed run skips this and leaves an orphan `Starting`
    // entry, which is what gets counted as a crash on the next
    // boot.)
    crash_recovery::record_clean_exit();
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

    /// v0.5.7 hotfix — `record_local_tray_snapshot` derives
    /// (month_so_far, predicted_total) from a `ScanResult` and stashes
    /// in `LAST_LOCAL_TRAY_VALUES`. Pin the round-trip:
    ///   - paired user with non-empty scan → values populated
    ///   - synthetic CLAUDE_MSG_BUCKET_MODEL rows are filtered (would
    ///     otherwise inflate the row count without contributing cost,
    ///     skewing the forecast's regression weight)
    ///
    /// VM verify 2026-05-06 caught the v0.5.6 regression where the
    /// tray read DASHBOARD_CACHE.daily_usage (only populated by
    /// Overview's poll). This unit test pins the contract that
    /// `record_local_tray_snapshot` writes the global the tray reads,
    /// so a future refactor can't silently break the wiring again.
    #[test]
    fn record_local_tray_snapshot_populates_global() {
        // Reset state so this test is order-independent (other tests
        // may run before us and leave stale values).
        if let Ok(mut g) = LAST_LOCAL_TRAY_VALUES.lock() {
            *g = None;
        }

        let today = chrono::Local::now().date_naive();
        // Synthesize a scan with one entry today + one synthetic
        // CLAUDE_MSG_BUCKET_MODEL row that should be filtered out.
        let today_key = today.format("%Y-%m-%d").to_string();
        let scan = scanner::ScanResult {
            entries: vec![
                scanner::DailyEntry {
                    date: today_key.clone(),
                    provider: "claude".to_string(),
                    model: "sonnet-4.6".to_string(),
                    input_tokens: 1000,
                    cached_tokens: 500,
                    output_tokens: 200,
                    cost_usd: Some(0.42),
                    message_count: 3,
                },
                scanner::DailyEntry {
                    date: today_key.clone(),
                    provider: "claude".to_string(),
                    model: scanner::CLAUDE_MSG_BUCKET_MODEL.to_string(),
                    input_tokens: 0,
                    cached_tokens: 0,
                    output_tokens: 0,
                    cost_usd: None,
                    message_count: 7,
                },
            ],
            total_cost_usd: 0.42,
            total_tokens: 1700,
            today_key: today_key.clone(),
            days_scanned: 30,
            files_scanned: 1,
            files_cached: 0,
        };
        record_local_tray_snapshot(&scan);

        let stashed = last_local_tray_values()
            .expect("LAST_LOCAL_TRAY_VALUES must be Some after a successful snapshot");
        // month_so_far should equal the cost of all non-bucket rows
        // for the current month (today only, in this fixture).
        assert!(
            (stashed.0 - 0.42).abs() < f64::EPSILON,
            "month_so_far expected ~0.42, got {}",
            stashed.0
        );
        // predicted_total is the forecast — must be at least
        // actual_to_date (forecast helper clamps lower-bound at the
        // already-realized cost).
        assert!(
            stashed.1 >= stashed.0,
            "predicted_total must be >= month_so_far, got {} vs {}",
            stashed.1,
            stashed.0
        );
    }

    /// v0.5.7 hotfix — empty scan (brand-new account, no usage yet)
    /// must still produce a stash; it's the FIRST sync that populates
    /// the value, and the user's tray would otherwise be stuck at "—"
    /// until they hit nonzero usage. The forecast helper handles
    /// zero-usage gracefully (returns is_reliable=false, all-zero
    /// values), and the tray formatter renders $0.00 for the zero
    /// case — meaningful and accurate.
    #[test]
    fn record_local_tray_snapshot_handles_empty_scan() {
        if let Ok(mut g) = LAST_LOCAL_TRAY_VALUES.lock() {
            *g = None;
        }

        let today_key = chrono::Local::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let scan = scanner::ScanResult {
            entries: vec![],
            total_cost_usd: 0.0,
            total_tokens: 0,
            today_key,
            days_scanned: 30,
            files_scanned: 0,
            files_cached: 0,
        };
        record_local_tray_snapshot(&scan);

        let stashed = last_local_tray_values();
        // Empty scan still produces a stash (forecast helper returns
        // Some(CostForecast { ..zeros }) for empty input — see
        // cost_forecast.rs:80-159, the only None path is invalid
        // reference_date which chrono can't produce).
        assert!(
            stashed.is_some(),
            "empty scan must still populate the stash"
        );
        let (month, forecast) = stashed.unwrap();
        assert_eq!(month, 0.0);
        assert_eq!(forecast, 0.0);
    }
}
