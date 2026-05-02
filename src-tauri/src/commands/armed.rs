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
    telemetry::send(&app, TelemetryEvent::PromptKilled);
}

/// True while a typing playback is in flight. Used by the tray Quit handler
/// to surface a confirm dialog instead of dropping mid-stream.
#[tauri::command]
#[specta::specta]
pub fn is_playing(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state.is_playing()
}
