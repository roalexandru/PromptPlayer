//! Picker IPC commands.

use crate::app::context::AppContext;
use crate::app::fire::{FireService, PickMode};
use crate::error::IpcResult;
use crate::picker::{SearchHit, RESTORATION_DELAY};
use crate::telemetry::{self, TelemetryEvent};
use tauri::{AppHandle, Manager};

#[tauri::command]
#[specta::specta]
pub fn picker_open(app: AppHandle, ctx: tauri::State<'_, AppContext>) -> IpcResult<()> {
    ctx.focus.capture();
    ctx.search
        .lock()
        .rebuild_if_stale(ctx.prompts.generation(), &ctx.prompts.read());
    #[cfg(target_os = "macos")]
    crate::platform::macos::position_picker_on_cursor_screen(&app);
    #[cfg(target_os = "windows")]
    crate::platform::windows::position_picker_on_cursor_screen(&app);
    show_picker_window(&app);
    telemetry::send(&app, TelemetryEvent::PickerOpened);
    Ok(())
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
    if !ctx.focus.restore() {
        tracing::warn!("focus restore failed");
    }
    std::thread::sleep(RESTORATION_DELAY);
    let fire = FireService::new(ctx.inner().clone(), app.clone());
    fire.fire_from_picker(&prompt_id, PickMode::parse(&mode));
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
