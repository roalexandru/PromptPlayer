//! Armed-state IPC commands.

use crate::app::shortcuts;
use crate::state::AppState;
use crate::telemetry::{self, TelemetryEvent};
use std::sync::Arc;
use tauri::AppHandle;

#[tauri::command]
#[specta::specta]
pub fn get_armed(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state.is_armed()
}

#[tauri::command]
#[specta::specta]
pub fn toggle_armed(app: AppHandle, state: tauri::State<'_, Arc<AppState>>) -> bool {
    let new = state.toggle_armed();
    shortcuts::refresh_tray_popup(&app);
    telemetry::send(&app, TelemetryEvent::ArmToggled { armed: new });
    new
}

#[tauri::command]
#[specta::specta]
pub fn kill(app: AppHandle, state: tauri::State<'_, Arc<AppState>>) {
    state.cancel_playback();
    // §2.7 — same red flash as the global kill-switch, so an abort from the
    // tray's "Stop typing" row is as visible as one from the hotkey.
    crate::app::tray_flash::flash_kill(&app);
    telemetry::send(&app, TelemetryEvent::PromptKilled);
}

/// True while a typing playback is in flight. Used by the tray Quit handler
/// to surface a confirm dialog instead of dropping mid-stream.
#[tauri::command]
#[specta::specta]
pub fn is_playing(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state.is_playing()
}

/// True iff the platform keyboard hook is currently installed and dispatching
/// events. False on macOS when Accessibility permission is missing or the tap
/// failed to install. The tray popup uses this to surface a "Grant Accessibility"
/// row instead of silently failing.
#[tauri::command]
#[specta::specta]
pub fn is_hook_alive(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state.hook_alive()
}

/// Open the macOS System Settings → Privacy & Security → Accessibility pane and
/// re-prompt. The prompt call adds this app to the Accessibility list (if not
/// already present) so the user has something to toggle when the pane opens.
/// On Windows this is a no-op (no equivalent permission system).
#[tauri::command]
#[specta::specta]
pub fn open_accessibility_settings() {
    #[cfg(target_os = "macos")]
    {
        // Trigger the prompt first — this adds us to the Accessibility list if
        // we aren't already there, so the System Settings pane has the right
        // entry visible when the user lands on it.
        let _ = crate::tcc::prompt_for_accessibility();
        crate::tcc::open_accessibility_settings();
    }
}
