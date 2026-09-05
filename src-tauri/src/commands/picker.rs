//! Picker IPC commands.

use crate::app::context::AppContext;
use crate::app::fire::{FireService, PickMode};
use crate::error::{into_ipc, AppError, IpcResult};
use crate::picker::{SearchHit, RESTORATION_DELAY, RESTORATION_TIMEOUT};
use crate::prompts::placeholders::{scan_stops, PromptStop};
use crate::telemetry::{self, TelemetryEvent};
use crate::usage::RECENTS_LIMIT;
use std::collections::HashMap;
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
    // Placement honours `picker-display:` — on an extended desktop the default
    // keeps the picker off the projector (see `picker::window::position_picker`).
    crate::picker::window::position_picker(app, ctx.config.get().picker_display);
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
    let mut hits = {
        let mut idx = ctx.search.lock();
        idx.rebuild_if_stale(ctx.prompts.generation(), &ctx.prompts.read());
        idx.query(&q, limit)
    };
    // With no query there is no relevance signal, so fall back to frecency
    // (§5.2's recents tier). A typed query already ranks by match score and
    // must not be re-sorted underneath the user.
    if q.trim().is_empty() {
        crate::picker::search::promote_recents(&mut hits, &ctx.usage.top(RECENTS_LIMIT));
    }
    hits
}

/// Tab stops and choices in a prompt body, for the picker's inline resolver.
///
/// §6.4 rules out a modal popup mid-expansion and says choices resolve "via
/// the picker UI itself before the picker dismisses". This is what the picker
/// asks for to render that; an empty result means "nothing to ask, fire it".
#[tauri::command]
#[specta::specta]
pub fn prompt_stops(
    prompt_id: String,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<Vec<PromptStop>> {
    match ctx.prompts.find(&prompt_id) {
        Some(p) => into_ipc(Ok(scan_stops(&p.body))),
        None => into_ipc(Err(AppError::PromptNotFound(prompt_id))),
    }
}

#[tauri::command]
#[specta::specta]
pub fn picker_select(
    app: AppHandle,
    prompt_id: String,
    mode: String,
    // Answers for the prompt's tab stops / choices, keyed by stop index.
    answers: Option<HashMap<String, String>>,
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
            fire.fire_from_picker_with(&prompt_id, mode, answers.unwrap_or_default());
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
        // (Re-)assert the picker's screen-capture exclusion on every show, on
        // BOTH platforms. `prepare_picker` lost its only caller in the
        // 1d33436 refactor, which left §5.4's default-on stealth guarantee
        // unimplemented everywhere; the Windows path was restored first, and
        // macOS (`NSWindow.sharingType = .none`) was still dormant. Idempotent
        // and one syscall, so it runs synchronously before `show()` — the
        // window is excluded before its first frame is ever composited.
        // Failures are logged by the platform helper (including the Win11
        // win32k bug and its WDA_MONITOR fallback).
        if let Err(e) = crate::picker::window::apply_screen_capture_exclusion(&w, true) {
            tracing::warn!("picker capture-exclusion failed: {e}");
        }
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        use tauri::Emitter;
        let _ = w.emit("picker-shown", ());
    }
}
