//! Window lifecycle: close-to-hide, and focus-loss for the picker and tray
//! popup. Focus restore lives in `commands::picker::dismiss` only — doing it
//! from both ends double-activates the previous app, visibly.

use tauri::{AppHandle, Emitter, Manager, WebviewWindow, WindowEvent};

/// A window came on screen. Webviews start visible-only work here.
pub const WINDOW_SHOWN: &str = "window-shown";
/// A window went off screen but is still alive — stop visible-only work.
pub const WINDOW_HIDDEN: &str = "window-hidden";
/// The prompt store changed. Replaces the library window's 2s poll.
pub const LIBRARY_CHANGED: &str = "library-changed";
/// The armed flag changed from anywhere (tray, hotkey, IPC).
pub const ARMED_CHANGED: &str = "armed-changed";

/// Emit to exactly one window. `Emitter::emit` on a `WebviewWindow` still
/// broadcasts to every webview, so anything window-scoped has to say so.
pub fn emit_to_window(
    app: &AppHandle,
    label: &str,
    event: &str,
    payload: impl serde::Serialize + Clone,
) {
    if let Err(e) = app.emit_to(label, event, payload) {
        tracing::debug!("emit {event} to {label} failed: {e}");
    }
}

/// Tell a window it is now on screen. Tauri builds every configured window at
/// startup regardless of `visible: false`, so a webview that starts a timer in
/// `onMount` polls from launch to exit unless it is told when to stand down.
pub fn notify_shown(app: &AppHandle, label: &str) {
    emit_to_window(app, label, WINDOW_SHOWN, ());
}

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
            // The webview stays alive after a hide, so nothing else tells it to
            // stop work that only matters while it is on screen.
            emit_to_window(&app, &label_owned, WINDOW_HIDDEN, ());
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
