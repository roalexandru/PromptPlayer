//! §8.4 — keyboard listener with suppression.
//!
//! On macOS we use a native `CGEventTap` (see `macos.rs`) because rdev's
//! `string_from_code` calls `TSMGetInputSourceProperty` from the tap callback
//! thread and SIGTRAPs on newer macOS.
//!
//! On Windows we keep `rdev::grab` (which uses `SetWindowsHookEx` under the hood
//! and doesn't have the same TSM issue).

use crate::matcher::MatcherState;
use crate::state::AppState;
use crate::undo::UndoLog;
use std::sync::Arc;
use std::time::Instant;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[derive(Debug, Clone)]
pub enum HookDecision {
    Pass,
    Suppress,
}

pub struct HookHandle {
    _t: std::marker::PhantomData<()>,
}

/// Spawn the platform-specific hook in its own thread.
pub fn spawn_grabbing_hook(
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    on_fire: Arc<dyn Fn(Vec<crate::matcher::PromptId>, String) + Send + Sync>,
    on_undo: Arc<dyn Fn() + Send + Sync>,
) -> HookHandle {
    #[cfg(target_os = "macos")]
    {
        spawn_macos(matcher, undo, app_state, on_fire, on_undo);
    }
    #[cfg(target_os = "windows")]
    {
        spawn_windows(matcher, undo, app_state, on_fire, on_undo);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (matcher, undo, app_state, on_fire, on_undo);
    }
    HookHandle {
        _t: std::marker::PhantomData,
    }
}

#[cfg(target_os = "macos")]
fn spawn_macos(
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    on_fire: Arc<dyn Fn(Vec<crate::matcher::PromptId>, String) + Send + Sync>,
    on_undo: Arc<dyn Fn() + Send + Sync>,
) {
    use macos::{KeyEvent, EventHandler};
    let handler: EventHandler = Arc::new(move |evt: KeyEvent| {
        process_event_native(&evt, &matcher, &undo, &app_state, &on_fire, &on_undo)
    });
    macos::spawn(handler);
}

#[cfg(target_os = "macos")]
fn process_event_native(
    evt: &macos::KeyEvent,
    matcher: &Arc<MatcherState>,
    undo: &Arc<UndoLog>,
    app_state: &Arc<AppState>,
    on_fire: &Arc<dyn Fn(Vec<crate::matcher::PromptId>, String) + Send + Sync>,
    on_undo: &Arc<dyn Fn() + Send + Sync>,
) -> Option<()> {
    if !app_state.is_armed() {
        tracing::debug!("hook: disarmed; passing through");
        return Some(());
    }
    if crate::secure_input::is_active() {
        tracing::warn!("hook: SECURE INPUT active; passing through (typing in a password-protected field?)");
        return Some(());
    }
    tracing::info!(
        "hook armed: typed={:?} buf_size={} commit='{}' last={:?}",
        evt.typed,
        matcher.buffer.read().len(),
        app_state.commit_char(),
        matcher.last_char()
    );

    let now = Instant::now();

    if evt.is_backspace {
        if undo.take_recent(now).is_some() {
            (on_undo)();
            return None;
        }
        matcher.observe_backspace(now);
        // Backspace still counts toward the §2.6 panic ring during playback.
        if app_state.is_playing() && app_state.record_cancel_keystroke(now) {
            app_state.cancel_playback();
        }
        return Some(());
    }

    // §2.6 — during playback, count *any* key down (printable or not) toward
    // the panic-abort ring. Pure modifier presses arrive as
    // `KCG_EVENT_FLAGS_CHANGED` and are filtered out by the tap callback in
    // `hook/macos.rs` (which only forwards `KCG_EVENT_KEY_DOWN`), so we don't
    // need to exclude them here.
    if app_state.is_playing() {
        if app_state.record_cancel_keystroke(now) {
            app_state.cancel_playback();
        }
        if let Some(c) = evt.typed {
            matcher.observe_char(c, now);
        }
        return Some(());
    }

    let Some(c) = evt.typed else {
        return Some(());
    };

    let global_commit = app_state.commit_char();
    if c == global_commit {
        if matcher.last_char() == Some('\\') {
            matcher.pop_last_chars(1);
            return Some(());
        }
        let candidates = matcher.try_match_all(c, now);
        if !candidates.is_empty() {
            let typed_form = candidates[0].typed_form.clone();
            let trigger_chars = candidates[0].trigger_chars;
            let candidate_ids: Vec<String> =
                candidates.into_iter().map(|m| m.prompt_id).collect();
            matcher.pop_last_chars(trigger_chars);
            (on_fire)(candidate_ids, typed_form);
            return None;
        }
        matcher.observe_char(c, now);
        return Some(());
    }
    matcher.observe_char(c, now);
    Some(())
}

#[cfg(target_os = "windows")]
fn spawn_windows(
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    on_fire: Arc<dyn Fn(Vec<crate::matcher::PromptId>, String) + Send + Sync>,
    on_undo: Arc<dyn Fn() + Send + Sync>,
) {
    std::thread::Builder::new()
        .name("prompt-player-hook".into())
        .spawn(move || {
            tracing::info!("hook thread starting (rdev/Win)");
            let result = rdev::grab(move |event: rdev::Event| {
                process_event_rdev(event, &matcher, &undo, &app_state, &on_fire, &on_undo)
            });
            if let Err(e) = result {
                tracing::error!("hook errored: {:?}", e);
            }
        })
        .expect("spawn hook thread");
}

#[cfg(target_os = "windows")]
fn process_event_rdev(
    event: rdev::Event,
    matcher: &Arc<MatcherState>,
    undo: &Arc<UndoLog>,
    app_state: &Arc<AppState>,
    on_fire: &Arc<dyn Fn(Vec<crate::matcher::PromptId>, String) + Send + Sync>,
    on_undo: &Arc<dyn Fn() + Send + Sync>,
) -> Option<rdev::Event> {
    if !app_state.is_armed() {
        return Some(event);
    }
    let now = Instant::now();
    let key = match &event.event_type {
        rdev::EventType::KeyPress(k) => Some(*k),
        _ => None,
    };
    if matches!(key, Some(rdev::Key::Backspace)) {
        if undo.take_recent(now).is_some() {
            (on_undo)();
            return None;
        }
        matcher.observe_backspace(now);
        // Backspace still counts toward the §2.6 panic ring during playback.
        if app_state.is_playing() && app_state.record_cancel_keystroke(now) {
            app_state.cancel_playback();
        }
        return Some(event);
    }
    // §2.6 — during playback, any non-modifier key down (printable or not)
    // counts toward the panic-abort ring. We filter pure modifiers here because
    // rdev surfaces them as ordinary `KeyPress` events.
    let is_pure_modifier = matches!(
        key,
        Some(rdev::Key::ShiftLeft)
            | Some(rdev::Key::ShiftRight)
            | Some(rdev::Key::ControlLeft)
            | Some(rdev::Key::ControlRight)
            | Some(rdev::Key::Alt)
            | Some(rdev::Key::AltGr)
            | Some(rdev::Key::MetaLeft)
            | Some(rdev::Key::MetaRight)
    );
    if app_state.is_playing() && key.is_some() && !is_pure_modifier {
        if app_state.record_cancel_keystroke(now) {
            app_state.cancel_playback();
        }
    }
    let ch = match (&event.event_type, &event.name) {
        (rdev::EventType::KeyPress(_), Some(name)) => name.chars().find(|c| !c.is_control()),
        _ => None,
    };
    if let Some(c) = ch {
        if app_state.is_playing() {
            // Panic ring already updated above; just keep the matcher buffer
            // in sync and pass the keystroke through.
            matcher.observe_char(c, now);
            return Some(event);
        }
        let global_commit = app_state.commit_char();
        if c == global_commit {
            if matcher.last_char() == Some('\\') {
                matcher.pop_last_chars(1);
                return Some(event);
            }
            let candidates = matcher.try_match_all(c, now);
            if !candidates.is_empty() {
                let typed_form = candidates[0].typed_form.clone();
                let trigger_chars = candidates[0].trigger_chars;
                let candidate_ids: Vec<String> =
                    candidates.into_iter().map(|m| m.prompt_id).collect();
                matcher.pop_last_chars(trigger_chars);
                (on_fire)(candidate_ids, typed_form);
                return None;
            }
            matcher.observe_char(c, now);
            return Some(event);
        }
        matcher.observe_char(c, now);
    }
    Some(event)
}
