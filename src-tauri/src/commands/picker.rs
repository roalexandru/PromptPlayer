//! Picker IPC commands.

use crate::app::context::AppContext;
use crate::app::fire::{FireService, PickMode};
use crate::error::IpcResult;
use crate::picker::{SearchHit, RESTORATION_DELAY, RESTORATION_TIMEOUT};
use crate::telemetry::{self, TelemetryEvent};
use tauri::{AppHandle, Manager};

#[tauri::command]
#[specta::specta]
pub fn picker_open(app: AppHandle, ctx: tauri::State<'_, AppContext>) -> IpcResult<()> {
    summon_picker(&app, &ctx);
    Ok(())
}

/// Full picker-open sequence shared by the `picker_open` IPC, the global
/// shortcut, the tray menu, and the single-instance relaunch handler — one
/// code path so behavior is identical regardless of entry point.
///
/// Skips the focus capture when the picker is already visible: re-summoning
/// while open must NOT overwrite the snapshot with Prompt Player itself
/// (which becomes frontmost the moment the picker shows). Otherwise the
/// eventual select would "restore" focus to Prompt Player and type into the
/// void.
///
/// Must be called on the main thread — the positioning calls use AppKit
/// (`NSEvent.mouseLocation`, `NSScreen.screens`) which require it.
pub fn summon_picker(app: &AppHandle, ctx: &AppContext) {
    let already_visible = app
        .get_webview_window("picker")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    if !already_visible {
        ctx.focus.capture();
    }
    ctx.search
        .lock()
        .rebuild_if_stale(ctx.prompts.generation(), &ctx.prompts.read());
    #[cfg(target_os = "macos")]
    crate::platform::macos::position_picker_on_cursor_screen(app);
    #[cfg(target_os = "windows")]
    crate::platform::windows::position_picker_on_cursor_screen(app);
    show_picker_window(app);
    telemetry::send(app, TelemetryEvent::PickerOpened);
}

#[tauri::command]
#[specta::specta]
pub fn picker_search(
    q: String,
    limit: Option<u32>,
    ctx: tauri::State<'_, AppContext>,
) -> Vec<SearchHit> {
    let limit = limit.unwrap_or(50) as usize;
    let mut idx = ctx.search.lock();
    idx.rebuild_if_stale(ctx.prompts.generation(), &ctx.prompts.read());
    idx.query(&q, limit)
}

#[tauri::command]
#[specta::specta]
pub fn picker_select(
    app: AppHandle,
    prompt_id: String,
    mode: String,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<()> {
    if let Some(w) = app.get_webview_window("picker") {
        let _ = w.hide();
    }
    // The focus restore busy-polls for up to RESTORATION_TIMEOUT (+ a fallback
    // nap). Tauri runs sync commands on the main/event-loop thread, so doing
    // that wait here would freeze the UI and the very run loop that processes
    // the app-deactivation we're waiting on. Offload to a worker; return to
    // the webview immediately.
    let ctx_owned = ctx.inner().clone();
    let app_owned = app.clone();
    let mode = PickMode::parse(&mode);
    std::thread::Builder::new()
        .name("prompt-player-picker-select".into())
        .spawn(move || {
            // Restore focus and wait until the OS actually reports the
            // previously-foreground window as foreground again. Paste mode
            // synthesizes Ctrl/Cmd+V which dispatches to *whoever* has focus
            // right now, so guessing with a blind sleep is what produced
            // "first chars land in the wrong window". The wait returns as soon
            // as the transfer is observed (usually <20ms) and falls back to a
            // small nap only if the verify loop times out.
            if !ctx_owned.focus.restore_and_wait(RESTORATION_TIMEOUT) {
                tracing::warn!(
                    "focus restore did not confirm within {:?}; falling back to blind delay",
                    RESTORATION_TIMEOUT
                );
                std::thread::sleep(RESTORATION_DELAY);
            }
            let fire = FireService::new(ctx_owned, app_owned);
            fire.fire_from_picker(&prompt_id, mode);
        })
        .expect("spawn picker-select thread");
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn picker_dismiss(app: AppHandle, ctx: tauri::State<'_, AppContext>) -> IpcResult<()> {
    if let Some(w) = app.get_webview_window("picker") {
        let _ = w.hide();
    }
    if !ctx.focus.restore() {
        tracing::warn!("focus restore on picker dismiss failed");
    }
    telemetry::send(&app, TelemetryEvent::PickerDismissed);
    Ok(())
}

/// Bring the picker window forward. Used by both the global shortcut path and
/// the tray-menu's "Command palette…" item — same code path so behavior is
/// identical regardless of how it was summoned.
pub fn show_picker_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("picker") {
        #[cfg(target_os = "macos")]
        crate::platform::macos::activate_app();
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        use tauri::Emitter;
        let _ = w.emit("picker-shown", ());
    }
}
