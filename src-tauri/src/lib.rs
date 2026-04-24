//! CLI Pulse Desktop — Tauri backend entry point.
//!
//! Sprint 0: local JSONL scan + per-day/model/provider aggregation.
//! Sprint 1 (this): Supabase pairing, config persistence, helper_sync
//! + upsert_daily_usage round-trips, periodic 2-minute sync tick.

pub mod config;
pub mod creds;
pub mod notify;
pub mod paths;
pub mod pricing;
pub mod scanner;
pub mod sessions;
pub mod supabase;
pub mod tray;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use config::HelperConfig;
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
    config::clear().map_err(|e| e.to_string())
}

#[derive(Debug, Serialize)]
struct SyncReport {
    sessions_synced: i64,
    alerts_synced: i64,
    metrics_uploaded: usize,
    total_cost_usd: f64,
    total_tokens: i64,
    files_scanned: u32,
    live_sessions_sent: usize,
    live_processes_seen: usize,
}

#[tauri::command]
async fn sync_now() -> Result<SyncReport, String> {
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

    // 2. helper_sync — ship live sessions + empty alerts/quotas (Sprint 2.5)
    let hs = supabase::helper_sync(&supabase::HelperSyncRequest {
        p_device_id: &cfg.device_id,
        p_helper_secret: &cfg.helper_secret,
        p_sessions: sessions::sessions_payload(&snapshot),
        p_alerts: json!([]),
        p_provider_remaining: json!({}),
        p_provider_tiers: json!({}),
    })
    .await
    .map_err(friendly)?;

    // 3. upsert_daily_usage
    let metrics: Vec<_> = scan
        .entries
        .iter()
        .filter_map(supabase::DailyUsageMetric::from_entry)
        .collect();
    let metrics_len = metrics.len();
    supabase::upsert_daily_usage(metrics)
        .await
        .map_err(friendly)?;

    Ok(SyncReport {
        sessions_synced: hs.sessions_synced,
        alerts_synced: hs.alerts_synced,
        metrics_uploaded: metrics_len,
        total_cost_usd: scan.total_cost_usd,
        total_tokens: scan.total_tokens,
        files_scanned: scan.files_scanned,
        live_sessions_sent: snapshot.sessions.len(),
        live_processes_seen: snapshot.total_processes_seen,
    })
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
            match background_tick().await {
                Ok(Some(report)) => {
                    log::info!(
                        "background sync ok — {} sessions, {} alerts, {} metrics",
                        report.sessions_synced,
                        report.alerts_synced,
                        report.metrics_uploaded
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
                    if consecutive_failures == SYNC_FAILURE_NOTIFY_THRESHOLD {
                        notify::sync_failure_streak(&app, consecutive_failures, &e);
                    }
                }
            }
            tokio::time::sleep(SYNC_INTERVAL).await;
        }
    });
}

async fn background_tick() -> Result<Option<SyncReport>, String> {
    // If we're not paired, this is a no-op.
    let cfg_exists = config::load().map_err(|e| e.to_string())?.is_some();
    if !cfg_exists {
        return Ok(None);
    }
    let report = sync_now().await?;
    Ok(Some(report))
}

// ------------------------------------------------------------------------
// Tauri entry
// ------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_bg = stop.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    // Let the background loop exit cleanly on app shutdown.
    stop.store(true, Ordering::Relaxed);
}

// silence `Value` / `json` unused if not referenced elsewhere
#[allow(dead_code)]
fn _json_ref_placeholder(_v: Value) {}
