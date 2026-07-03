//! "Keep Awake" IPC commands — mirror the `armed` toggle wiring.

use crate::app::context::AppContext;
use crate::app::shortcuts;
use crate::telemetry::{self, TelemetryEvent};
use tauri::AppHandle;

/// Current keep-awake state. Read by the macOS tray popover on every show.
#[tauri::command]
#[specta::specta]
pub fn get_keep_awake(ctx: tauri::State<'_, AppContext>) -> bool {
    ctx.power.is_enabled()
}

/// Flip keep-awake, apply the OS-level assertion, and return the new state.
/// Refreshes the tray popover (macOS) so the checkmark stays in sync; the
/// Windows native menu is rebuilt from a fresh snapshot on every open, so it
/// needs no explicit refresh.
#[tauri::command]
#[specta::specta]
pub fn toggle_keep_awake(app: AppHandle, ctx: tauri::State<'_, AppContext>) -> bool {
    let enabled = ctx.power.toggle();
    shortcuts::refresh_tray_popup(&app);
    telemetry::send(&app, TelemetryEvent::KeepAwakeToggled { enabled });
    enabled
}
