//! Tray-menu IPC commands.

use crate::app::context::AppContext;
use crate::commands::picker::FocusCapture;
use crate::error::{into_ipc, AppError, IpcResult};
use crate::telemetry::PickerSource;
use tauri::{AppHandle, Manager};

#[cfg(target_os = "macos")]
use crate::platform::macos as plat;
#[cfg(target_os = "windows")]
use crate::platform::windows as plat;

#[tauri::command]
#[specta::specta]
pub fn tray_open(
    app: AppHandle,
    target: String,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<()> {
    match target.as_str() {
        "library" => {
            show_window(&app, "library");
            Ok(())
        }
        "picker" => {
            // One shared sequence (capture, reindex, reposition, show,
            // report) — this used to be a third copy that emitted nothing.
            crate::commands::picker::summon_picker(
                &app,
                &ctx,
                PickerSource::TrayMenu,
                FocusCapture::Take,
            );
            Ok(())
        }
        "about" => {
            // A real window, not a system MessageDialog: that renders a generic
            // warning icon with no room for version or update controls.
            show_window(&app, "about");
            Ok(())
        }
        other => into_ipc(Err(AppError::InvalidArg(format!(
            "unknown tray target: {other}"
        )))),
    }
}

#[tauri::command]
#[specta::specta]
pub fn tray_quit(app: AppHandle) {
    app.exit(0);
}

/// Fire a pinned prompt from the macOS popover. The tray click never activates
/// us, so the user's app still has focus and needs no restore.
#[tauri::command]
#[specta::specta]
pub fn tray_fire_prompt(
    app: AppHandle,
    prompt_id: String,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<()> {
    if let Some(w) = app.get_webview_window("tray-popup") {
        let _ = w.hide();
    }
    #[cfg(target_os = "macos")]
    remove_outside_click_monitor_if_present(&app);
    let fire = crate::app::FireService::new(ctx.inner().clone(), app.clone());
    fire.fire_from_tray(&prompt_id, crate::app::fire::PickMode::Human);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn tray_popup_hide(app: AppHandle) -> IpcResult<()> {
    if let Some(w) = app.get_webview_window("tray-popup") {
        let _ = w.hide();
    }
    #[cfg(target_os = "macos")]
    remove_outside_click_monitor_if_present(&app);
    Ok(())
}

fn show_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        plat::activate_app();
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        // `.accessory` apps have no Dock icon to fall back on, so focus transfer
        // can silently fail; `orderFrontRegardless` raises it without activating.
        #[cfg(target_os = "macos")]
        crate::platform::macos::order_window_front_regardless(&w);
        crate::app::lifecycle::notify_shown(app, label);
    }
}

/// Show or hide the tray popup — a native menu on Windows, where webview popups
/// have no reliable outside-click dismiss, and an NSPanel on macOS.
pub fn toggle_popup(app: &AppHandle, rect: tauri::Rect) {
    #[cfg(target_os = "windows")]
    {
        // Modal — blocks until the user picks or dismisses, so there's no
        // "already visible" state to track. Win32 owns it.
        crate::platform::windows::menu::show_tray_menu(app, rect);
        return;
    }

    #[cfg(target_os = "macos")]
    {
        let Some(window) = app.get_webview_window("tray-popup") else {
            return;
        };
        let already_visible = window.is_visible().unwrap_or(false);
        if already_visible {
            let _ = window.hide();
            remove_outside_click_monitor_if_present(app);
            return;
        }
        position_popup(&window, rect);
        use tauri::Emitter;
        let _ = window.emit("tray-popup-show", ());
        let _ = window.show();
        plat::order_panel_front_no_activate(&window);
        install_outside_click_monitor_if_needed(app);
    }
}

#[cfg(target_os = "macos")]
fn position_popup(window: &tauri::WebviewWindow, _rect: tauri::Rect) {
    // Mixed-DPI monitors can overlap in physical pixels, so `set_position`
    // picks the wrong screen; AppKit's logical points are one unified space.
    crate::platform::macos::position_popover_under_cursor(window);
}

#[cfg(target_os = "macos")]
fn install_outside_click_monitor_if_needed(_app: &AppHandle) {
    use std::sync::Arc;
    if let Some(monitor) = _app.try_state::<Arc<crate::platform::macos::OutsideClickMonitor>>() {
        crate::platform::macos::install_outside_click_monitor(_app, monitor.inner());
    }
}

#[cfg(target_os = "macos")]
fn remove_outside_click_monitor_if_present(_app: &AppHandle) {
    use std::sync::Arc;
    if let Some(monitor) = _app.try_state::<Arc<crate::platform::macos::OutsideClickMonitor>>() {
        crate::platform::macos::remove_outside_click_monitor(monitor.inner());
    }
}
