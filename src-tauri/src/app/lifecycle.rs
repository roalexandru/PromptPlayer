//! Window lifecycle: close-to-hide, and focus-loss for the picker and tray
//! popup. Focus restore lives in `commands::picker::dismiss` only — doing it
//! from both ends double-activates the previous app, visibly.

use tauri::{AppHandle, Manager, WebviewWindow, WindowEvent};

#[cfg(target_os = "macos")]
use crate::platform::macos::OutsideClickMonitor;
#[cfg(target_os = "macos")]
use std::sync::Arc;
// Windows uses a native HMENU, so the `tray-popup` webview never shows there
// and this handler is mac-only.

pub fn install(app: &tauri::App) {
    // Diagnostics is close-to-hide like the rest: destroying it makes the
    // tray's "Diagnostics…" item a one-shot for the rest of the run.
    for label in ["library", "picker", "tray-popup", "about", "diagnostics"] {
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
            // Hide only. The Esc/select IPC paths own restoration, and doing it
            // here would yank focus back from whatever app the user just clicked.
            let _ = w_clone.hide();
        }
        _ => {}
    });
}
