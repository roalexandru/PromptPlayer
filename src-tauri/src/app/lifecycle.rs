//! Window lifecycle hooks: close-to-hide, focus-loss for the picker and
//! tray-popup.
//!
//! Picker dismiss-on-focus-loss intentionally does NOT call
//! `focus.restore()` here — that's handled in `commands::picker::dismiss`.
//! Calling it from both ends produces a double-activate of the previously-
//! focused app, which on slow machines or apps with activation animations
//! is visible.

use crate::picker::FocusStore;
use std::sync::Arc;
use tauri::{AppHandle, Manager, WebviewWindow, WindowEvent};

#[cfg(target_os = "macos")]
use crate::platform::macos::OutsideClickMonitor;

pub fn install(app: &tauri::App) {
    for label in ["library", "picker", "settings", "tray-popup"] {
        if let Some(w) = app.get_webview_window(label) {
            install_window_handlers(app.handle().clone(), label, w);
        }
    }
}

fn install_window_handlers(app: AppHandle, label: &str, window: WebviewWindow) {
    let label_owned = label.to_string();
    let w_clone = window.clone();
    window.on_window_event(move |e| match e {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let _ = w_clone.hide();
        }
        WindowEvent::Focused(false) if label_owned == "tray-popup" => {
            let _ = w_clone.hide();
            #[cfg(target_os = "macos")]
            if let Some(monitor) = app.try_state::<Arc<OutsideClickMonitor>>() {
                crate::platform::macos::remove_outside_click_monitor(monitor.inner());
            }
        }
        WindowEvent::Focused(false) if label_owned == "picker" => {
            // Hide on focus loss. Focus restoration is intentionally NOT done
            // here — the explicit dismiss path (Esc / outside click handled
            // elsewhere) calls `FocusStore::restore`. Doing it here too would
            // double-activate the previously-focused app.
            let _ = w_clone.hide();
            // Single restore here covers the click-outside-the-picker case
            // where no explicit IPC dismiss fires.
            if let Some(focus) = app.try_state::<Arc<FocusStore>>() {
                let _ = focus.restore();
            }
        }
        _ => {}
    });
}
