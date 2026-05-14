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
    // Restore focus and wait until the OS actually reports the previously-
    // foreground window as foreground again. Paste mode synthesizes Ctrl/Cmd+V
    // which dispatches to *whoever* has focus right now, so guessing with a
    // blind sleep is what produced "first chars land in the wrong window".
    // The wait returns as soon as the transfer is observed (usually <20ms)
    // and falls back to a small nap only if the verify loop times out.
    if !ctx.focus.restore_and_wait(RESTORATION_TIMEOUT) {
        tracing::warn!(
            "focus restore did not confirm within {:?}; falling back to blind delay",
            RESTORATION_TIMEOUT
        );
        std::thread::sleep(RESTORATION_DELAY);
    }
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

        // Windows-only: WebView2 lazily creates its GPU swap-chain child
        // HWND on first paint, not at window-show. The synchronous
        // `prepare_picker` call only saw HWNDs that existed at that
        // moment; the swap chain that appears moments later misses the
        // recursive `WDA_EXCLUDEFROMCAPTURE` walk. Re-apply once after a
        // single paint cycle so any late-spawned descendants get the
        // flag too. 150 ms is conservative — typical WebView2 first-paint
        // is <50 ms even on cold start. No-op if the window is gone.
        //
        // Use `std::thread::spawn` rather than `tokio::spawn`: this
        // function is invoked from global-shortcut + tray-menu callbacks
        // that don't always run inside a tokio runtime context, and
        // `tokio::spawn` panics outside one. A short-lived sleep thread
        // is fine — the cost is one thread that lives ~150ms.
        #[cfg(target_os = "windows")]
        {
            let app = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                if let Some(w) = app.get_webview_window("picker") {
                    if let Err(e) = crate::picker::window::apply_screen_capture_exclusion(&w, true)
                    {
                        tracing::warn!("deferred capture-exclusion re-apply failed: {e}");
                    }
                }
            });
        }
    }
}
