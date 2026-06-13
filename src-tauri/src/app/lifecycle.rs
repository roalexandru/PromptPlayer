//! Window lifecycle hooks: close-to-hide, focus-loss for the picker and
//! tray-popup.
//!
//! Picker dismiss-on-focus-loss intentionally does NOT call
//! `focus.restore()` here — that's handled in `commands::picker::dismiss`.
//! Calling it from both ends produces a double-activate of the previously-
//! focused app, which on slow machines or apps with activation animations
//! is visible.

use tauri::{AppHandle, Manager, WebviewWindow, WindowEvent};

#[cfg(target_os = "macos")]
use crate::platform::macos::OutsideClickMonitor;
#[cfg(target_os = "macos")]
use std::sync::Arc;
// Windows uses a native `TrackPopupMenuEx` HMENU for the tray popup — the
// `tray-popup` webview is never shown on Windows, so the focus-loss handler
// below is mac-only.

pub fn install(app: &tauri::App) {
    for label in ["library", "picker", "tray-popup", "about"] {
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
            // Hide ONLY — never restore focus here. The explicit Esc/select
            // IPC paths (`picker_dismiss` / `picker_select`) own restoration;
            // restoring here too double-activated the previous app on every
            // dismiss. And when the picker loses focus because the user
            // clicked into a *different* app, the OS already moved focus
            // there — restoring would yank it back to the stale snapshot,
            // stealing focus from the app the user just chose.
            let _ = w_clone.hide();
        }
        _ => {}
    });
}
