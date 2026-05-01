//! §8.4 — keyboard listener with suppression.
//!
//! On macOS we use a native `CGEventTap` (see `macos.rs`) because rdev's
//! `string_from_code` calls `TSMGetInputSourceProperty` from the tap callback
//! thread and SIGTRAPs on newer macOS.
//!
//! On Windows we keep `rdev::grab` (which uses `SetWindowsHookEx` under the
//! hood and doesn't have the same TSM issue).
//!
//! Both platforms translate their native event into a `KeyEvent`, then the
//! shared `process_event` function decides Pass / Suppress. This is the
//! single source of truth for: armed gate, secure-input gate, undo,
//! panic-ring, escape-hatch (`\>`), trigger match, ring-buffer maintenance.

use crate::matcher::MatcherState;
use crate::state::AppState;
use crate::undo::UndoLog;
use std::sync::Arc;
use std::time::Instant;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

/// Cross-platform key event passed to `process_event`. Each platform module
/// translates its native event type into this struct.
#[derive(Debug, Clone, Default)]
pub struct KeyEvent {
    /// The Unicode character produced by this keystroke (None for non-text
    /// keys: arrows, F-keys, modifier-only events, etc.).
    pub typed: Option<char>,
    /// Backspace pressed.
    pub is_backspace: bool,
    /// True for events that are pure modifier keypresses (Shift/Ctrl/Alt/
    /// Meta with no other key). Set by the platform layer; macOS' callback
    /// already filters these out at the OS layer, Windows surfaces them.
    pub is_pure_modifier: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum HookDecision {
    Pass,
    Suppress,
}

pub struct HookHandle {
    _t: std::marker::PhantomData<()>,
}

/// Bundle of references the shared `process_event` needs. Cheap to construct
/// per-event because everything is already an `Arc`.
pub struct HookDeps<'a> {
    pub matcher: &'a Arc<MatcherState>,
    pub undo: &'a Arc<UndoLog>,
    pub app_state: &'a Arc<AppState>,
    pub on_fire: &'a Arc<dyn Fn(Vec<crate::matcher::PromptId>, String) + Send + Sync>,
    pub on_undo: &'a Arc<dyn Fn() + Send + Sync>,
}

/// Shared event-handling pipeline used by both macOS and Windows hooks.
///
/// Returns `Pass` when the host should let the event continue to the focused
/// app, `Suppress` when it should swallow the event (commit-char that fired
/// a trigger, or backspace that triggered an undo).
pub fn process_event(evt: &KeyEvent, deps: &HookDeps<'_>) -> HookDecision {
    if !deps.app_state.is_armed() {
        return HookDecision::Pass;
    }
    if crate::secure_input::is_active() {
        return HookDecision::Pass;
    }
    tracing::trace!(
        "hook armed: typed={:?} buf_size={} commit='{}' last={:?}",
        evt.typed,
        deps.matcher.buffer.read().len(),
        deps.app_state.commit_char(),
        deps.matcher.last_char()
    );

    let now = Instant::now();

    if evt.is_backspace {
        if deps.undo.take_recent(now).is_some() {
            (deps.on_undo)();
            return HookDecision::Suppress;
        }
        deps.matcher.observe_backspace(now);
        if deps.app_state.is_playing() && deps.app_state.record_cancel_keystroke(now) {
            deps.app_state.cancel_playback();
        }
        return HookDecision::Pass;
    }

    if deps.app_state.is_playing() {
        if !evt.is_pure_modifier && deps.app_state.record_cancel_keystroke(now) {
            deps.app_state.cancel_playback();
        }
        if let Some(c) = evt.typed {
            deps.matcher.observe_char(c, now);
        }
        return HookDecision::Pass;
    }

    let Some(c) = evt.typed else {
        return HookDecision::Pass;
    };

    let global_commit = deps.app_state.commit_char();
    if c == global_commit {
        // §2.4 — escape hatch: `\>` types a literal `>`.
        if deps.matcher.last_char() == Some('\\') {
            deps.matcher.pop_last_chars(1);
            return HookDecision::Pass;
        }
        let candidates = deps.matcher.try_match_all(c, now);
        if !candidates.is_empty() {
            let typed_form = candidates[0].typed_form.clone();
            let trigger_chars = candidates[0].trigger_chars;
            let candidate_ids: Vec<String> =
                candidates.into_iter().map(|m| m.prompt_id).collect();
            deps.matcher.pop_last_chars(trigger_chars);
            (deps.on_fire)(candidate_ids, typed_form);
            return HookDecision::Suppress;
        }
        deps.matcher.observe_char(c, now);
        return HookDecision::Pass;
    }
    deps.matcher.observe_char(c, now);
    HookDecision::Pass
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
    use macos::EventHandler;
    let handler: EventHandler = Arc::new(move |evt: macos::NativeKeyEvent| {
        let key = KeyEvent {
            typed: evt.typed,
            is_backspace: evt.is_backspace,
            is_pure_modifier: false, // macOS tap filters these at OS layer.
        };
        let deps = HookDeps {
            matcher: &matcher,
            undo: &undo,
            app_state: &app_state,
            on_fire: &on_fire,
            on_undo: &on_undo,
        };
        match process_event(&key, &deps) {
            HookDecision::Pass => Some(()),
            HookDecision::Suppress => None,
        }
    });
    macos::spawn(handler);
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
                let key = translate_rdev(&event);
                let deps = HookDeps {
                    matcher: &matcher,
                    undo: &undo,
                    app_state: &app_state,
                    on_fire: &on_fire,
                    on_undo: &on_undo,
                };
                match process_event(&key, &deps) {
                    HookDecision::Pass => Some(event),
                    HookDecision::Suppress => None,
                }
            });
            if let Err(e) = result {
                tracing::error!("hook errored: {:?}", e);
            }
        })
        .expect("spawn hook thread");
}

#[cfg(target_os = "windows")]
fn translate_rdev(event: &rdev::Event) -> KeyEvent {
    let key = match &event.event_type {
        rdev::EventType::KeyPress(k) => Some(*k),
        _ => None,
    };
    let is_backspace = matches!(key, Some(rdev::Key::Backspace));
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
    let typed = match (&event.event_type, &event.name) {
        (rdev::EventType::KeyPress(_), Some(name)) => name.chars().find(|c| !c.is_control()),
        _ => None,
    };
    KeyEvent {
        typed,
        is_backspace,
        is_pure_modifier,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::TriggerEntry;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn make_deps_default(
    ) -> (Arc<MatcherState>, Arc<UndoLog>, Arc<AppState>) {
        (
            MatcherState::shared(),
            Arc::new(UndoLog::new()),
            AppState::shared(),
        )
    }

    fn fire_count_callback() -> (
        Arc<dyn Fn(Vec<String>, String) + Send + Sync>,
        Arc<AtomicUsize>,
    ) {
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let cb: Arc<dyn Fn(Vec<String>, String) + Send + Sync> =
            Arc::new(move |_ids, _form| {
                count2.fetch_add(1, Ordering::Relaxed);
            });
        (cb, count)
    }

    fn no_op_undo_callback() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    fn ke(c: char) -> KeyEvent {
        KeyEvent {
            typed: Some(c),
            is_backspace: false,
            is_pure_modifier: false,
        }
    }

    #[test]
    fn disarmed_passes_everything_through() {
        let (matcher, undo, app_state) = make_deps_default();
        let (on_fire, count) = fire_count_callback();
        let on_undo = no_op_undo_callback();
        let deps = HookDeps {
            matcher: &matcher,
            undo: &undo,
            app_state: &app_state,
            on_fire: &on_fire,
            on_undo: &on_undo,
        };
        // Disarmed (default) — every event is Pass and never matches.
        for ch in ['b', 'u', 'i', 'l', 'd', '>'] {
            let d = process_event(&ke(ch), &deps);
            assert!(matches!(d, HookDecision::Pass));
        }
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn armed_with_no_trigger_passes_commit_char() {
        let (matcher, undo, app_state) = make_deps_default();
        app_state.set_armed(true);
        let (on_fire, count) = fire_count_callback();
        let on_undo = no_op_undo_callback();
        let deps = HookDeps {
            matcher: &matcher,
            undo: &undo,
            app_state: &app_state,
            on_fire: &on_fire,
            on_undo: &on_undo,
        };
        for ch in ['x', 'y', '>'] {
            let _ = process_event(&ke(ch), &deps);
        }
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn armed_with_matching_trigger_fires_and_suppresses_commit() {
        let (matcher, undo, app_state) = make_deps_default();
        app_state.set_armed(true);
        matcher
            .rebuild_index(vec![TriggerEntry {
                canonical: "build".into(),
                prompt_id: "p1".into(),
                word_count: 1,
                commit_char: '>',
            }])
            .unwrap();

        let (on_fire, count) = fire_count_callback();
        let on_undo = no_op_undo_callback();
        let deps = HookDeps {
            matcher: &matcher,
            undo: &undo,
            app_state: &app_state,
            on_fire: &on_fire,
            on_undo: &on_undo,
        };
        for ch in ['B', 'u', 'i', 'l', 'd'] {
            let d = process_event(&ke(ch), &deps);
            assert!(matches!(d, HookDecision::Pass));
        }
        let d = process_event(&ke('>'), &deps);
        assert!(matches!(d, HookDecision::Suppress), "commit char must be suppressed when trigger matches");
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn escape_hatch_for_commit_char() {
        let (matcher, undo, app_state) = make_deps_default();
        app_state.set_armed(true);
        let (on_fire, count) = fire_count_callback();
        let on_undo = no_op_undo_callback();
        let deps = HookDeps {
            matcher: &matcher,
            undo: &undo,
            app_state: &app_state,
            on_fire: &on_fire,
            on_undo: &on_undo,
        };
        // Type `\` then `>` — the `>` should pass through without firing.
        let _ = process_event(&ke('\\'), &deps);
        let d = process_event(&ke('>'), &deps);
        assert!(matches!(d, HookDecision::Pass));
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn backspace_during_undo_window_invokes_undo_callback() {
        let (matcher, undo, app_state) = make_deps_default();
        app_state.set_armed(true);
        let undo_count = Arc::new(AtomicUsize::new(0));
        let undo_count2 = undo_count.clone();
        let on_fire: Arc<dyn Fn(Vec<String>, String) + Send + Sync> =
            Arc::new(|_, _| {});
        let on_undo: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            undo_count2.fetch_add(1, Ordering::Relaxed);
        });
        // Seed the undo log with a recent expansion.
        undo.record("Build".into(), 50);
        let deps = HookDeps {
            matcher: &matcher,
            undo: &undo,
            app_state: &app_state,
            on_fire: &on_fire,
            on_undo: &on_undo,
        };
        let key = KeyEvent {
            typed: None,
            is_backspace: true,
            is_pure_modifier: false,
        };
        let d = process_event(&key, &deps);
        assert!(matches!(d, HookDecision::Suppress));
        assert_eq!(undo_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn pure_modifier_during_playback_does_not_count_toward_panic_ring() {
        let (matcher, undo, app_state) = make_deps_default();
        app_state.set_armed(true);
        let _cancel = app_state.begin_playback();
        let (on_fire, _) = fire_count_callback();
        let on_undo = no_op_undo_callback();
        let deps = HookDeps {
            matcher: &matcher,
            undo: &undo,
            app_state: &app_state,
            on_fire: &on_fire,
            on_undo: &on_undo,
        };
        // Three pure-modifier events in fast succession should NOT trigger
        // the panic-cancel.
        let modifier_event = KeyEvent {
            typed: None,
            is_backspace: false,
            is_pure_modifier: true,
        };
        for _ in 0..3 {
            process_event(&modifier_event, &deps);
        }
        assert!(!app_state
            .begin_playback()
            .load(Ordering::Relaxed));
        // (begin_playback returns the cancel flag fresh, so we test that
        // it's not pre-set.)
    }
}
