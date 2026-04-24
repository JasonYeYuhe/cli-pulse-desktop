//! CLI Pulse Desktop — Tauri backend entry point.
//!
//! Sprint 0 scope: expose `scan_usage` command that scans local JSONL logs,
//! returns per-day / per-model token + cost breakdown to the React frontend.
//! No network, no UI state persistence yet.

pub mod paths;
pub mod pricing;
pub mod scanner;

use scanner::ScanResult;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! CLI Pulse Desktop is running.", name)
}

#[tauri::command]
fn scan_usage(days: Option<u32>) -> Result<ScanResult, String> {
    let days = days.unwrap_or(30).clamp(1, 180);
    scanner::scan(days).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet, scan_usage])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
