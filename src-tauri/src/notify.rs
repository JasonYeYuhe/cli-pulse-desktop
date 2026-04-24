//! Native desktop notifications via `tauri-plugin-notification`.
//!
//! Used sparingly — the macOS app's in-process heuristic is "notify on
//! state change that the user cares about, never on every tick." Sprint
//! 2.5 wires two triggers:
//!
//!   1. Pair success (one-time reassurance)
//!   2. Sync failure streak ≥ 3 (so the user doesn't silently miss
//!      uploads — the iOS/macOS apps show stale data otherwise)
//!
//! Permission model: on macOS/Linux the plugin handles permission
//! requests implicitly; on Windows the first notify call triggers the
//! OS-level permission dialog. We don't block on permission grant —
//! failures are logged but never surfaced to the user.

use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

pub fn pair_success<R: Runtime>(app: &AppHandle<R>, device_name: &str) {
    send(
        app,
        "CLI Pulse — Paired",
        &format!("Device “{device_name}” is now syncing with your phone."),
    );
}

pub fn sync_failure_streak<R: Runtime>(app: &AppHandle<R>, consecutive: u32, err: &str) {
    let short: String = err.chars().take(140).collect();
    send(
        app,
        "CLI Pulse — Sync paused",
        &format!("{consecutive} consecutive sync failures.\n{short}"),
    );
}

/// Fired once per (day, budget kind) — see `maybe_notify_budget_breach`
/// in lib.rs for the de-dup logic. `title` and `body` are produced by
/// `alerts::compute` and trusted to be human-readable.
pub fn budget_breach<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    // Truncate extremely long messages so Windows Action Center / macOS
    // Notification Center don't refuse the payload.
    let body: String = body.chars().take(280).collect();
    send(app, title, &body);
}

fn send<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    let result = app.notification().builder().title(title).body(body).show();
    if let Err(e) = result {
        log::warn!("notification send failed: {e}");
    }
}
