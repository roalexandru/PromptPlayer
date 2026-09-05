//! Picker IPC commands.

use crate::app::context::AppContext;
use crate::app::fire::{FireService, PickMode};
use crate::error::IpcResult;
use crate::picker::{SearchHit, RESTORATION_DELAY, RESTORATION_TIMEOUT};
use crate::telemetry::{self, PickerSource, TelemetryEvent};
use tauri::{AppHandle, Manager};

#[tauri::command]
#[specta::specta]
pub fn picker_open(app: AppHandle, ctx: tauri::State<'_, AppContext>) -> IpcResult<()> {
    summon_picker(&app, &ctx, PickerSource::Ipc, FocusCapture::Take);
    Ok(())
}

/// Whether `summon_picker` should snapshot the foreground app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusCapture {
    Take,
    /// The caller already captured — the Windows native menu does this in
    /// `run_menu`, before its helper window takes the foreground.
    AlreadyTaken,
}

/// The one picker-open sequence — three copies existed and only this reported.
/// Skips focus capture while visible, and must run on the main thread.
pub fn summon_picker(
    app: &AppHandle,
    ctx: &AppContext,
    source: PickerSource,
    capture: FocusCapture,
) {
    let already_visible = app
        .get_webview_window("picker")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    if !already_visible && capture == FocusCapture::Take {
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
    telemetry::send(app, TelemetryEvent::PickerOpened { source });
}

#[tauri::command]
#[specta::specta]
pub fn picker_search(
    q: String,
    limit: Option<u32>,
    ctx: tauri::State<'_, AppContext>,
) -> Vec<SearchHit> {
    let limit = limit.unwrap_or(50) as usize;
    // Length only, never content — reported once the search ends.
    ctx.picker_search.note(q.chars().count());
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
    report_search_chars(&app, &ctx);
    // Focus restore busy-polls, and sync commands run on the event loop — the
    // very loop processing the deactivation we're waiting for. Offload it.
    let ctx_owned = ctx.inner().clone();
    let app_owned = app.clone();
    let mode = PickMode::parse(&mode);
    std::thread::Builder::new()
        .name("prompt-player-picker-select".into())
        .spawn(move || {
            // Wait for the OS to confirm: Ctrl/Cmd+V goes to whoever has focus
            // *now*, and a blind sleep landed chars in the wrong window.
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
    report_search_chars(&app, &ctx);
    telemetry::send(&app, TelemetryEvent::PickerDismissed);
    Ok(())
}

/// Flush the peak search length for the picker session that just ended.
fn report_search_chars(app: &AppHandle, ctx: &AppContext) {
    if let Some(chars_typed) = ctx.picker_search.take() {
        telemetry::send(app, TelemetryEvent::PickerSearchChars { chars_typed });
    }
}

/// Show the picker when there's no `AppContext` to summon it with — only the
/// single-instance fallback, which runs before setup registers state.
pub fn summon_picker_without_context(app: &AppHandle) {
    show_picker_window(app);
    telemetry::send(
        app,
        TelemetryEvent::PickerOpened {
            source: PickerSource::Relaunch,
        },
    );
}

/// Bring the picker window forward. Private on purpose: every caller must go
/// through a `summon_*` wrapper, so a new entry point cannot skip reporting.
fn show_picker_window(app: &AppHandle) {
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
