//! Armed-state IPC commands.

use crate::app::context::AppContext;
use crate::app::shortcuts;
use crate::state::AppState;
use crate::telemetry::{self, CancelReason, TelemetryEvent};
use std::sync::Arc;
use tauri::AppHandle;

#[tauri::command]
#[specta::specta]
pub fn get_armed(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state.is_armed()
}

#[tauri::command]
#[specta::specta]
pub fn toggle_armed(app: AppHandle, ctx: tauri::State<'_, AppContext>) -> bool {
    let new = ctx.state.toggle_armed();
    shortcuts::set_armed_and_report(&app, &ctx, new);
    new
}

#[tauri::command]
#[specta::specta]
pub fn kill(app: AppHandle, state: tauri::State<'_, Arc<AppState>>) {
    // Read before cancelling: `was_playing=false` means the user hit the kill
    // switch with nothing in flight, which reads nothing like a real abort.
    let was_playing = state.is_playing();
    state.cancel_playback_with(CancelReason::Kill);
    // §2.7 — same red flash as the global kill-switch, so an abort from the
    // tray's "Stop typing" row is as visible as one from the hotkey.
    crate::app::tray_flash::flash_kill(&app);
    telemetry::send(&app, TelemetryEvent::PromptKilled { was_playing });
}

/// True while a typing playback is in flight. Used by the tray Quit handler
/// to surface a confirm dialog instead of dropping mid-stream.
#[tauri::command]
#[specta::specta]
pub fn is_playing(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state.is_playing()
}

/// Whether the keyboard hook is installed and dispatching. The tray uses it to
/// warn instead of failing silently.
#[tauri::command]
#[specta::specta]
pub fn is_hook_alive(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state.hook_alive()
}

/// Open the macOS Accessibility pane, re-prompting first so we're listed there
/// and the user has something to toggle. No-op on Windows.
#[tauri::command]
#[specta::specta]
pub fn open_accessibility_settings() {
    #[cfg(target_os = "macos")]
    {
        // Prompt first so we're in the Accessibility list before the pane opens.
        let _ = crate::tcc::prompt_for_accessibility();
        crate::tcc::open_accessibility_settings();
    }
}

/// `tccutil reset Accessibility`, then re-prompt and open the pane. This is the
/// fix for the "approved but not working" state an unsigned update leaves.
#[tauri::command]
#[specta::specta]
pub async fn reset_accessibility(app: AppHandle) -> bool {
    telemetry::send(&app, TelemetryEvent::AccessibilityReset);
    // `async`, because a synchronous Tauri command runs on the main thread and
    // this waits on `tccutil` — which froze the whole UI, including the
    // diagnostics window whose button started it.
    let reset = tauri::async_runtime::spawn_blocking(|| {
        let ok = crate::tcc::reset_accessibility(crate::tcc::BUNDLE_ID);
        if ok {
            // Re-register in the list, then send the user to the toggle.
            let _ = crate::tcc::prompt_for_accessibility();
            crate::tcc::open_accessibility_settings();
        }
        ok
    })
    .await;
    match reset {
        Ok(ok) => ok,
        Err(e) => {
            tracing::error!("reset_accessibility task failed: {e}");
            false
        }
    }
}
