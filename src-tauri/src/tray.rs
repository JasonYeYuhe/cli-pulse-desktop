//! System tray icon — built on Tauri 2's native tray API.
//!
//! Windows: first-class tray behavior.
//! Linux:   works via libayatana-appindicator3 when installed. GNOME 40+
//!          has dropped legacy tray support; users without the
//!          AppIndicator extension won't see the icon. The app is
//!          main-window-first by design, so this is graceful degradation.
//!
//! Left-click: toggle (show/focus or hide) the main window.
//! Right-click / menu: rich content + Open / Refresh now / Quit.
//!
//! v0.5.6 — tray menu now shows live mini-metrics:
//!   - Month so far: $X.XX
//!   - Forecast:     $Y.YY
//!   - Synced N ago
//!
//! Update primitive (Tauri 2 cross-platform reality, both reviewers
//! independently flagged): `MenuItem::set_text()` on stored handles,
//! NOT `tray.set_menu(Some(rebuild))`. Linux AppIndicator-backed menus
//! cannot be removed/replaced after first set; both Win and Linux
//! dismiss any open right-click menu when `set_menu` is called. Mutating
//! the existing items in-place is the only primitive that works on both.
//!
//! Refresh cadence: 120 s. Tied to the same cadence as the main
//! background_sync loop so we naturally never race a cache miss against
//! an in-flight `dashboard_summary` fetch (the existing 30 s-TTL
//! `DASHBOARD_CACHE` in lib.rs:573 is the read source — tray never
//! triggers a fresh network fetch). See lib.rs::spawn_tray_refresh_loop.

use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

/// Live metrics for the tray menu's three dynamic rows. lib.rs
/// constructs this from the existing 30 s-TTL `DASHBOARD_CACHE` +
/// the cost-forecast helper + a global "last successful sync"
/// timestamp.
///
/// All fields `Option` because the tray must render reasonable copy
/// pre-pairing (no JWT yet → no dashboard data) AND on first-launch
/// before any background_tick has run. The localized strings for the
/// "—" / "Not paired yet" cases live in i18n on the frontend; the
/// tray reads runtime-localized copy via `TrayCopy` below to stay
/// consistent with the user's app-language choice.
#[derive(Debug, Clone, Default)]
pub struct TrayMetrics {
    pub month_so_far_usd: Option<f64>,
    pub forecast_usd: Option<f64>,
    pub synced_seconds_ago: Option<u64>,
    pub paired: bool,
}

/// Fully-localized strings for the tray menu items. The frontend's
/// language-change handler invokes `force_tray_menu_refresh` which
/// rebuilds these from the active i18n language and calls
/// `set_text()` on the dynamic handles — tray copy flips immediately
/// without waiting for the next 120 s tick (fixes Gemini 3.1 Pro
/// pre-implementation P2: tray would otherwise render in the
/// previous language for up to 2 min after the user switched).
///
/// Scope note: v0.5.6 deliberately ships header / stats / Open /
/// Quit only. A "Refresh now" tray item would need cross-module
/// event plumbing to drive the actual sync; the in-app
/// Settings → "Sync now" button already covers that use case
/// without growing the v0.5.6 diff.
#[derive(Debug, Clone)]
pub struct TrayCopy {
    pub header_label: String,
    pub month_so_far_template: String, // contains literal "{value}"
    pub forecast_template: String,     // contains literal "{value}"
    pub synced_ago_template: String,   // contains literal "{age}"
    pub synced_never: String,
    pub not_paired: String,
    pub no_data: String,
    pub open_label: String,
    pub quit_label: String,
}

impl Default for TrayCopy {
    /// English fallback for the boot path before the frontend has
    /// pushed its language choice. Identical strings to the en.json
    /// `tray.*` keys so the user sees no flicker between
    /// `install_tray()` and the first `force_tray_menu_refresh`.
    fn default() -> Self {
        Self {
            header_label: "CLI Pulse".to_string(),
            month_so_far_template: "Month so far: {value}".to_string(),
            forecast_template: "Forecast: {value}".to_string(),
            synced_ago_template: "Synced {age} ago".to_string(),
            synced_never: "Not synced yet".to_string(),
            not_paired: "Not signed in".to_string(),
            no_data: "—".to_string(),
            open_label: "Open CLI Pulse".to_string(),
            quit_label: "Quit".to_string(),
        }
    }
}

/// Stored handles for the dynamic menu items. Tauri's `MenuItem`
/// is `Send + Sync` via internal `Arc`, so storing the handles in
/// `app.manage(...)` lets the 120 s refresh loop AND the
/// `force_tray_menu_refresh` Tauri command both read the same
/// handles without an extra Mutex around the items themselves.
/// The Mutex wraps the `TrayCopy` so we can swap localized strings
/// atomically when the frontend pushes a language change.
pub struct TrayDynamicHandles<R: Runtime> {
    header_item: MenuItem<R>,
    month_item: MenuItem<R>,
    forecast_item: MenuItem<R>,
    synced_item: MenuItem<R>,
    open_item: MenuItem<R>,
    quit_item: MenuItem<R>,
    copy: Mutex<TrayCopy>,
}

pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let copy = TrayCopy::default();
    let header = MenuItem::with_id(app, "header", &copy.header_label, false, None::<&str>)?;
    let month_item = MenuItem::with_id(
        app,
        "month",
        format_month_so_far(&copy, None, false),
        false,
        None::<&str>,
    )?;
    let forecast_item = MenuItem::with_id(
        app,
        "forecast",
        format_forecast(&copy, None, false),
        false,
        None::<&str>,
    )?;
    let synced_item = MenuItem::with_id(
        app,
        "synced",
        format_synced_ago(&copy, None),
        false,
        None::<&str>,
    )?;

    let open_item = MenuItem::with_id(app, "open", &copy.open_label, true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", &copy.quit_label, true, None::<&str>)?;

    // `PredefinedMenuItem::separator(app)` produces a native
    // separator. We sandwich the three dynamic rows between
    // separators so they read as a distinct "stats" block.
    use tauri::menu::PredefinedMenuItem;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &header,
            &sep1,
            &month_item,
            &forecast_item,
            &synced_item,
            &sep2,
            &open_item,
            &quit_item,
        ],
    )?;

    // Stash all 7 menu-item handles so the refresh loop + the
    // language-change Tauri command can mutate them in-place. Header
    // and the open/refresh/quit triplet are stored too so the
    // language-flip path can re-localize their labels alongside the
    // dynamic stats rows — without these, switching languages would
    // leave "Open CLI Pulse" / "Refresh now" / "Quit" stuck in
    // whichever language was active at install time.
    app.manage(TrayDynamicHandles {
        header_item: header.clone(),
        month_item: month_item.clone(),
        forecast_item: forecast_item.clone(),
        synced_item: synced_item.clone(),
        open_item: open_item.clone(),
        quit_item: quit_item.clone(),
        copy: Mutex::new(copy),
    });

    TrayIconBuilder::with_id("main")
        .tooltip("CLI Pulse")
        .icon(app.default_window_icon().cloned().unwrap_or_else(|| {
            // Fallback — embed the 32x32 at compile time if no window icon
            // is registered. Shouldn't happen since tauri.conf.json lists
            // icons, but keeps us robust.
            tauri::image::Image::new(&[], 0, 0)
        }))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Apply fresh `TrayMetrics` to the dynamic menu items, using the
/// currently-stored localized copy. Called from two places:
///   1. The 120 s background loop (lib.rs::spawn_tray_refresh_loop)
///   2. The `force_tray_menu_refresh` Tauri command
///
/// No-op when `TrayDynamicHandles` isn't installed yet (e.g. the
/// pre-paired Linux user whose AppIndicator extension is missing,
/// where install() returned `Err` and we logged a warning). Caller
/// doesn't need to check `is_installed`.
pub fn apply_metrics<R: Runtime>(app: &AppHandle<R>, metrics: &TrayMetrics) {
    let Some(state) = app.try_state::<TrayDynamicHandles<R>>() else {
        return;
    };
    let copy = match state.copy.lock() {
        Ok(g) => g.clone(),
        Err(_) => {
            log::warn!("tray copy mutex poisoned; falling back to default English");
            TrayCopy::default()
        }
    };
    let _ = state.month_item.set_text(format_month_so_far(
        &copy,
        metrics.month_so_far_usd,
        metrics.paired,
    ));
    let _ =
        state
            .forecast_item
            .set_text(format_forecast(&copy, metrics.forecast_usd, metrics.paired));
    let _ = state
        .synced_item
        .set_text(format_synced_ago(&copy, metrics.synced_seconds_ago));
}

/// Replace the stored localized copy AND immediately re-render the
/// static-label menu items (header / open / refresh / quit) in the
/// new language. The dynamic stats rows are NOT re-rendered here —
/// callers are expected to follow this with `apply_metrics` so the
/// stats lines stay accurate on the language flip. The two-step
/// `set_copy` → `apply_metrics` pattern keeps the API explicit
/// about which fields can drift if you skip a step.
///
/// Called from `force_tray_menu_refresh` after the frontend has
/// pushed a language change. Without this, the next 120 s refresh
/// cycle would render stats in the new language but the
/// open/refresh/quit triplet would stay in the previous language
/// until app restart (Gemini 3.1 Pro pre-implementation P2 on
/// language desync).
pub fn set_copy<R: Runtime>(app: &AppHandle<R>, copy: TrayCopy) {
    let Some(state) = app.try_state::<TrayDynamicHandles<R>>() else {
        return;
    };
    let _ = state.header_item.set_text(&copy.header_label);
    let _ = state.open_item.set_text(&copy.open_label);
    let _ = state.quit_item.set_text(&copy.quit_label);
    // Inner block scopes the `MutexGuard` so it drops cleanly with
    // `state`. The trailing `let _ = ()` is a no-op statement that
    // forces the function body to NOT end on the if-let expression,
    // ensuring the lock temporary is dropped before the implicit
    // function-return-value tail drops `state`.
    {
        if let Ok(mut g) = state.copy.lock() {
            *g = copy;
        }
    }
    let _ = ();
}

// ============================================================
// Formatters — pure functions of (copy, metric). Unit-tested.
// ============================================================

fn format_usd(value: f64) -> String {
    format!("${value:.2}")
}

fn format_month_so_far(copy: &TrayCopy, value: Option<f64>, paired: bool) -> String {
    if !paired {
        return copy
            .month_so_far_template
            .replace("{value}", &copy.not_paired);
    }
    let v = match value {
        Some(v) => format_usd(v),
        None => copy.no_data.clone(),
    };
    copy.month_so_far_template.replace("{value}", &v)
}

fn format_forecast(copy: &TrayCopy, value: Option<f64>, paired: bool) -> String {
    if !paired {
        return copy.forecast_template.replace("{value}", &copy.not_paired);
    }
    let v = match value {
        Some(v) => format_usd(v),
        None => copy.no_data.clone(),
    };
    copy.forecast_template.replace("{value}", &v)
}

fn format_synced_ago(copy: &TrayCopy, seconds: Option<u64>) -> String {
    let Some(s) = seconds else {
        return copy.synced_never.clone();
    };
    let age = if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else if s < 86_400 {
        format!("{} hr", s / 3600)
    } else {
        format!("{} d", s / 86_400)
    };
    copy.synced_ago_template.replace("{age}", &age)
}

fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.unminimize();
    }
}

fn toggle_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        match w.is_visible() {
            Ok(true) => {
                let _ = w.hide();
            }
            _ => {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! v0.5.6 — formatter unit tests. The set_text contract is
    //! Tauri-internal and only meaningful under a real AppHandle, so
    //! we pin the formatter shape here (the only thing that affects
    //! what the user sees in the tray).

    use super::*;

    #[test]
    fn format_usd_two_decimals() {
        assert_eq!(format_usd(0.0), "$0.00");
        assert_eq!(format_usd(0.999), "$1.00"); // banker's rounding via fmt
        assert_eq!(format_usd(12.34), "$12.34");
        assert_eq!(format_usd(1234.5), "$1234.50");
    }

    #[test]
    fn month_so_far_unpaired_shows_not_paired() {
        let copy = TrayCopy::default();
        assert_eq!(
            format_month_so_far(&copy, Some(99.99), false),
            "Month so far: Not signed in"
        );
        // Paired but no data yet — show the dash, not the not-paired
        // hint (the user IS paired; the data just isn't there yet).
        assert_eq!(format_month_so_far(&copy, None, true), "Month so far: —");
    }

    #[test]
    fn forecast_paired_with_value() {
        let copy = TrayCopy::default();
        assert_eq!(format_forecast(&copy, Some(42.5), true), "Forecast: $42.50");
    }

    #[test]
    fn synced_ago_buckets_correctly() {
        let copy = TrayCopy::default();
        assert_eq!(format_synced_ago(&copy, None), "Not synced yet");
        assert_eq!(format_synced_ago(&copy, Some(0)), "Synced 0 s ago");
        assert_eq!(format_synced_ago(&copy, Some(45)), "Synced 45 s ago");
        assert_eq!(format_synced_ago(&copy, Some(60)), "Synced 1 min ago");
        assert_eq!(format_synced_ago(&copy, Some(3599)), "Synced 59 min ago");
        assert_eq!(format_synced_ago(&copy, Some(3600)), "Synced 1 hr ago");
        assert_eq!(format_synced_ago(&copy, Some(7200)), "Synced 2 hr ago");
        assert_eq!(format_synced_ago(&copy, Some(86_400)), "Synced 1 d ago");
    }

    /// v0.5.6 — i18n template substitution must be ALL replacements,
    /// not just the first. Tauri's `set_text` doesn't sanitize the
    /// input; if a translator accidentally repeats `{value}` in
    /// their string (unlikely for English, possible for languages
    /// where the placeholder needs to be repeated for grammar), we
    /// want every instance replaced — `String::replace` on stable
    /// Rust does that by default. Pin the contract.
    #[test]
    fn template_replace_handles_repeated_placeholder() {
        let copy = TrayCopy {
            month_so_far_template: "{value} ({value})".into(),
            ..TrayCopy::default()
        };
        assert_eq!(format_month_so_far(&copy, Some(7.0), true), "$7.00 ($7.00)");
    }

    /// v0.5.6 — the localized "synced ago" template must place {age}
    /// in the user's preferred grammatical position. zh-CN renders
    /// "已同步 {{age}} 前" with the age in the middle — verifying
    /// the substitution doesn't accidentally lose surrounding
    /// punctuation.
    #[test]
    fn template_replace_preserves_surrounding_text() {
        let copy = TrayCopy {
            synced_ago_template: "已同步 {age} 前".into(),
            ..TrayCopy::default()
        };
        assert_eq!(format_synced_ago(&copy, Some(120)), "已同步 2 min 前");
    }
}
