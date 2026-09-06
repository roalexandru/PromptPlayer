//! Picker IPC commands.

use crate::app::context::AppContext;
use crate::app::fire::{FireService, PickMode};
use crate::error::{into_ipc, AppError, IpcResult};
use crate::picker::{SearchHit, RESTORATION_DELAY, RESTORATION_TIMEOUT};
use crate::prompts::placeholders::{scan_stops, PromptStop};
use crate::telemetry::{self, PickerSource, TelemetryEvent};
use crate::usage::RECENTS_LIMIT;
use std::collections::HashMap;
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
    // Placement honours `picker-display:` — on an extended desktop the default
    // keeps the picker off the projector (see `picker::window::position_picker`).
    crate::picker::window::position_picker(app, ctx.config.get().picker_display);
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

/// Apply the capture exclusion and make anything short of full exclusion
/// visible. A `WDA_MONITOR` fallback still hides the content, but the audience
/// sees a black rectangle where the picker sits — and a presenter who believes
/// they are invisible needs telling. A log line mid-demo is not telling them.
fn report_capture_exclusion(app: &AppHandle, w: &tauri::WebviewWindow) {
    use crate::picker::window::CaptureExclusion;
    let degraded = match crate::picker::window::apply_screen_capture_exclusion(w, true) {
        Ok(CaptureExclusion::Full) => None,
        Ok(other) => {
            tracing::error!(
                "picker screen-capture exclusion is only partly in effect ({other:?}) — \
                 the picker may be visible to a screen share"
            );
            Some(crate::telemetry::CaptureDegradeReason::MonitorFallback)
        }
        Err(e) => {
            tracing::error!("picker capture-exclusion failed: {e}");
            Some(crate::telemetry::CaptureDegradeReason::Failed)
        }
    };
    let Some(ctx) = app.try_state::<AppContext>() else {
        return;
    };
    if ctx.attention.set_capture_degraded(degraded.is_some()) {
        crate::tray_icon::refresh(app);
    }
    if let Some(reason) = degraded {
        telemetry::send(app, TelemetryEvent::CaptureExclusionDegraded { reason });
    }
}

/// Bring the picker window forward. Private on purpose: every caller must go
/// through a `summon_*` wrapper, so a new entry point cannot skip reporting.
fn show_picker_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("picker") {
        #[cfg(target_os = "macos")]
        crate::platform::macos::activate_app();
        // Re-assert capture exclusion on every show, on BOTH platforms.
        // `prepare_picker` lost its only caller in the 1d33436 refactor, which
        // left §5.4's default-on stealth guarantee unimplemented everywhere;
        // the Windows path was restored first and macOS
        // (`NSWindow.sharingType = .none`) was still dormant. One idempotent
        // syscall, run before `show()` so the first frame is already excluded.
        report_capture_exclusion(app, &w);
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        use tauri::Emitter;
        let _ = w.emit("picker-shown", ());
    }
}
