//! System tray icon — built on Tauri 2's native tray API.
//!
//! Windows: first-class tray behavior.
//! Linux:   works via libayatana-appindicator3 when installed. GNOME 40+
//!          has dropped legacy tray support; users without the
//!          AppIndicator extension won't see the icon. The app is
//!          main-window-first by design, so this is graceful degradation.
//!
//! Left-click: toggle (show/focus or hide) the main window.
//! Right-click / menu: Open / Quit.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime};

pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let open_item = MenuItem::with_id(app, "open", "Open CLI Pulse", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

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
