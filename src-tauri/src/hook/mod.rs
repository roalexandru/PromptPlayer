//! §8.4 — keyboard listener with suppression. Each platform's native hook
//! filters our own injected keys, or playback trips the §2.6 panic ring.
//!
//! Both feed `KeyEvent` to `process_event`, the single source of truth for the
//! gates, undo, panic ring, `\>` escape and matching.

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
    /// A bare modifier press. macOS filters these at the OS layer; Windows
    /// surfaces them, so the platform layer sets this.
    pub is_pure_modifier: bool,
    /// Return/Tab: no character, but still a word boundary, so a trigger typed
    /// right after Enter still matches.
    pub is_separator: bool,
    /// Escape pressed. During playback this is an explicit "stop now" and
    /// gets its own `CancelReason` instead of counting toward the panic ring.
    pub is_escape: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum HookDecision {
    Pass,
    Suppress,
}

pub struct HookHandle {
    _t: std::marker::PhantomData<()>,
}

/// Host callbacks the hook invokes. One struct so adding a callback doesn't
/// mean editing five spawn signatures. Cheap to clone — all fields are `Arc`.
#[derive(Clone)]
pub struct HookCallbacks {
    pub on_fire: Arc<dyn Fn(Vec<crate::matcher::PromptId>, String) + Send + Sync>,
    pub on_undo: Arc<dyn Fn() + Send + Sync>,
    pub on_literal_commit: Arc<dyn Fn(char) + Send + Sync>,
    /// Every commit char typed while armed, matched or not.
    /// `(matched, matcher_index_size)`.
    pub on_commit_observed: Arc<dyn Fn(bool, usize) + Send + Sync>,
    /// A commit char typed while Secure Input had the hook gated shut — i.e.
    /// a trigger that silently did nothing.
    pub on_blocked_commit: Arc<dyn Fn() + Send + Sync>,
}

impl HookCallbacks {
    /// All-no-op set. Used by tests and by the non-mac/win stub build.
    pub fn noop() -> Self {
        Self {
            on_fire: Arc::new(|_, _| {}),
            on_undo: Arc::new(|| {}),
            on_literal_commit: Arc::new(|_| {}),
            on_commit_observed: Arc::new(|_, _| {}),
            on_blocked_commit: Arc::new(|| {}),
        }
    }
}

/// Bundle of references the shared `process_event` needs. Cheap to construct
/// per-event because everything is already an `Arc`.
pub struct HookDeps<'a> {
    pub matcher: &'a Arc<MatcherState>,
    pub undo: &'a Arc<UndoLog>,
    pub app_state: &'a Arc<AppState>,
    pub cb: &'a HookCallbacks,
    /// macOS Secure Event Input state, sampled by the platform handler; always
    /// false on Windows. An input (not an FFI call) so the gate is testable.
    pub secure_input_active: bool,
}

/// Shared pipeline for both platform hooks. `Suppress` swallows the event —
/// a commit char that fired, a backspace that undid, or Esc during playback.
pub fn process_event(evt: &KeyEvent, deps: &HookDeps<'_>) -> HookDecision {
    if !deps.app_state.is_armed() {
        return HookDecision::Pass;
    }
    // Secure-Input gate (mac-only): pass everything through untouched, but
    // still count a commit char so "the gate ate a trigger" is measurable.
    if deps.secure_input_active {
        if let Some(c) = evt.typed {
            if c == deps.app_state.commit_char() || deps.matcher.is_commit_char(c) {
                (deps.cb.on_blocked_commit)();
            }
        }
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
        // Peek only — `run_undo` is the sole consumer, and consuming here would
        // suppress the Backspace with no undo. During playback it feeds §2.6.
        if !deps.app_state.is_playing() && deps.undo.has_recent(now) {
            (deps.cb.on_undo)();
            return HookDecision::Suppress;
        }
        deps.matcher.observe_backspace(now);
        if deps.app_state.is_playing() && deps.app_state.record_cancel_keystroke(now) {
            deps.app_state.cancel_playback();
        }
        return HookDecision::Pass;
    }

    if deps.app_state.is_playing() {
        // Esc is the reflex abort: one press, and swallowed so the target app
        // doesn't also close the dialog the user was typing into.
        if evt.is_escape {
            tracing::info!("Esc during playback — aborting");
            deps.app_state
                .cancel_playback_with(crate::telemetry::CancelReason::Esc);
            return HookDecision::Suppress;
        }
        if !evt.is_pure_modifier && deps.app_state.record_cancel_keystroke(now) {
            deps.app_state
                .cancel_playback_with(crate::telemetry::CancelReason::UserKeystrokes);
        }
        if let Some(c) = evt.typed {
            deps.matcher.observe_char(c, now);
        } else if evt.is_separator {
            deps.matcher.observe_separator(now);
        }
        return HookDecision::Pass;
    }

    // Return / Tab carry no character but break the word run — without a
    // boundary, `<line>\nbuild>` glues into one token and never matches.
    if evt.is_separator {
        deps.matcher.observe_separator(now);
        return HookDecision::Pass;
    }

    let Some(c) = evt.typed else {
        return HookDecision::Pass;
    };

    let global_commit = deps.app_state.commit_char();
    // Per-prompt `commit-char:` overrides (§2.3) register their chars in the
    // matcher index; without this check only the global char could ever fire.
    if c == global_commit || deps.matcher.is_commit_char(c) {
        // §2.4 — escape hatch: `\>` types a literal `>`.
        if deps.matcher.last_char() == Some('\\') {
            deps.matcher.pop_last_chars(1);
            deps.matcher.observe_char(c, now);
            (deps.cb.on_literal_commit)(c);
            return HookDecision::Suppress;
        }
        let candidates = deps.matcher.try_match_all(c, now);
        // One line per user-typed `>`. `candidates=0` with a non-zero index
        // means the trigger text didn't match; 0/0 means nothing was loaded.
        let buf_len = deps.matcher.buffer.read().len();
        let index_len = deps.matcher.index.read().len();
        let matched = !candidates.is_empty();
        tracing::info!(
            "commit char '{}' observed: candidates={} buffer_len={} matcher_triggers={}",
            c,
            candidates.len(),
            buf_len,
            index_len,
        );
        // Separate from `on_fire` because the no-match case matters most:
        // `matched=false` is "the user expected a fire and got nothing".
        (deps.cb.on_commit_observed)(matched, index_len);
        if matched {
            let typed_form = candidates[0].typed_form.clone();
            // `pop_chars` runs to the buffer end so `build >` leaves no residue;
            // `trigger_chars` would strand the space and poison the next match.
            let pop_chars = candidates[0].pop_chars;
            let candidate_ids: Vec<String> = candidates.into_iter().map(|m| m.prompt_id).collect();
            tracing::info!(
                "fire: trigger='{}' candidates={:?}",
                typed_form,
                candidate_ids
            );
            deps.matcher.pop_last_chars(pop_chars);
            (deps.cb.on_fire)(candidate_ids, typed_form);
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
    cb: HookCallbacks,
) -> HookHandle {
    #[cfg(target_os = "macos")]
    {
        spawn_macos(matcher, undo, app_state, cb);
    }
    #[cfg(target_os = "windows")]
    {
        spawn_windows(matcher, undo, app_state, cb);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (matcher, undo, app_state, cb);
    }
    HookHandle {
        _t: std::marker::PhantomData,
    }
}

/// macOS virtual keycode for Escape.
#[cfg(target_os = "macos")]
const KEY_CODE_ESCAPE_MAC: u16 = 53;

#[cfg(target_os = "macos")]
fn spawn_macos(
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    cb: HookCallbacks,
) {
    use macos::EventHandler;
    let app_state_for_handler = app_state.clone();
    let handler: EventHandler = Arc::new(move |evt: macos::NativeKeyEvent| {
        // macOS virtual keycodes: Return=36, Tab=48, KeypadEnter=76, Esc=53.
        let is_separator = matches!(evt.keycode, 36 | 48 | 76);
        let key = KeyEvent {
            typed: evt.typed,
            is_backspace: evt.is_backspace,
            is_pure_modifier: false, // macOS tap filters these at OS layer.
            is_separator,
            is_escape: evt.keycode == KEY_CODE_ESCAPE_MAC,
        };
        let deps = HookDeps {
            matcher: &matcher,
            undo: &undo,
            app_state: &app_state_for_handler,
            cb: &cb,
            // Sampled per event: a cached value can be a poll interval stale,
            // and stale-in-the-wrong-direction means touching a password field.
            secure_input_active: crate::secure_input::is_active(),
        };
        match process_event(&key, &deps) {
            HookDecision::Pass => Some(()),
            HookDecision::Suppress => None,
        }
    });
    macos::spawn(handler, app_state);
}

/// Re-spawn the macOS hook when Accessibility flips on after launch.
/// Idempotent — `macos::spawn` re-checks permission itself.
#[cfg(target_os = "macos")]
pub fn respawn_macos(
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    cb: HookCallbacks,
) {
    spawn_macos(matcher, undo, app_state, cb);
}

#[cfg(target_os = "windows")]
fn spawn_windows(
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    cb: HookCallbacks,
) {
    // Native `WH_KEYBOARD_LL`, mirroring the CGEventTap design — injected
    // events are filtered here so the panic ring never sees playback chars.
    windows::spawn(matcher, undo, app_state, cb);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::TriggerEntry;
    use crate::telemetry::CancelReason;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Owns everything `HookDeps` borrows, plus a counter per callback.
    struct Harness {
        matcher: Arc<MatcherState>,
        undo: Arc<UndoLog>,
        app_state: Arc<AppState>,
        cb: HookCallbacks,
        fires: Arc<AtomicUsize>,
        undos: Arc<AtomicUsize>,
        literals: Arc<AtomicUsize>,
        commits: Arc<AtomicUsize>,
        blocked: Arc<AtomicUsize>,
        secure_input_active: bool,
    }

    impl Harness {
        fn new() -> Self {
            let fires = Arc::new(AtomicUsize::new(0));
            let undos = Arc::new(AtomicUsize::new(0));
            let literals = Arc::new(AtomicUsize::new(0));
            let commits = Arc::new(AtomicUsize::new(0));
            let blocked = Arc::new(AtomicUsize::new(0));
            let (f, u, l, c, b) = (
                fires.clone(),
                undos.clone(),
                literals.clone(),
                commits.clone(),
                blocked.clone(),
            );
            Self {
                matcher: MatcherState::shared(),
                undo: Arc::new(UndoLog::new()),
                app_state: AppState::shared(),
                cb: HookCallbacks {
                    on_fire: Arc::new(move |_ids, _form| {
                        f.fetch_add(1, Ordering::Relaxed);
                    }),
                    on_undo: Arc::new(move || {
                        u.fetch_add(1, Ordering::Relaxed);
                    }),
                    on_literal_commit: Arc::new(move |_c| {
                        l.fetch_add(1, Ordering::Relaxed);
                    }),
                    on_commit_observed: Arc::new(move |_matched, _n| {
                        c.fetch_add(1, Ordering::Relaxed);
                    }),
                    on_blocked_commit: Arc::new(move || {
                        b.fetch_add(1, Ordering::Relaxed);
                    }),
                },
                fires,
                undos,
                literals,
                commits,
                blocked,
                secure_input_active: false,
            }
        }

        fn armed() -> Self {
            let h = Self::new();
            h.app_state.set_armed(true);
            h
        }

        fn deps(&self) -> HookDeps<'_> {
            HookDeps {
                matcher: &self.matcher,
                undo: &self.undo,
                app_state: &self.app_state,
                cb: &self.cb,
                secure_input_active: self.secure_input_active,
            }
        }

        fn send(&self, evt: &KeyEvent) -> HookDecision {
            process_event(evt, &self.deps())
        }

        fn type_str(&self, s: &str) {
            for ch in s.chars() {
                self.send(&ke(ch));
            }
        }

        fn n(counter: &Arc<AtomicUsize>) -> usize {
            counter.load(Ordering::Relaxed)
        }
    }

    fn ke(c: char) -> KeyEvent {
        KeyEvent {
            typed: Some(c),
            ..Default::default()
        }
    }

    fn backspace() -> KeyEvent {
        KeyEvent {
            is_backspace: true,
            ..Default::default()
        }
    }

    fn esc() -> KeyEvent {
        KeyEvent {
            is_escape: true,
            ..Default::default()
        }
    }

    fn build_trigger() -> TriggerEntry {
        TriggerEntry {
            canonical: "build".into(),
            prompt_id: "p1".into(),
            word_count: 1,
            commit_char: '>',
        }
    }

    #[test]
    fn disarmed_passes_everything_through() {
        let h = Harness::new();
        for ch in ['b', 'u', 'i', 'l', 'd', '>'] {
            assert!(matches!(h.send(&ke(ch)), HookDecision::Pass));
        }
        assert_eq!(Harness::n(&h.fires), 0);
    }

    #[test]
    fn armed_with_no_trigger_passes_commit_char() {
        let h = Harness::armed();
        h.type_str("xy>");
        assert_eq!(Harness::n(&h.fires), 0);
        assert_eq!(Harness::n(&h.commits), 1, "no-match commits still report");
    }

    #[test]
    fn armed_with_matching_trigger_fires_and_suppresses_commit() {
        let h = Harness::armed();
        h.matcher.rebuild_index(vec![build_trigger()]);
        for ch in ['B', 'u', 'i', 'l', 'd'] {
            assert!(matches!(h.send(&ke(ch)), HookDecision::Pass));
        }
        assert!(
            matches!(h.send(&ke('>')), HookDecision::Suppress),
            "commit char must be suppressed when trigger matches"
        );
        assert_eq!(Harness::n(&h.fires), 1);
        assert_eq!(Harness::n(&h.commits), 1);
    }

    #[test]
    fn escape_hatch_for_commit_char() {
        // `\>` types a literal commit char instead of firing.
        let h = Harness::armed();
        h.send(&ke('\\'));
        assert!(matches!(h.send(&ke('>')), HookDecision::Suppress));
        assert_eq!(Harness::n(&h.fires), 0);
        assert_eq!(Harness::n(&h.literals), 1);
        assert_eq!(h.matcher.last_char(), Some('>'));
    }

    #[test]
    fn backspace_during_undo_window_invokes_undo_callback() {
        let h = Harness::armed();
        h.undo.record("Build".into(), 50);
        assert!(matches!(h.send(&backspace()), HookDecision::Suppress));
        assert_eq!(Harness::n(&h.undos), 1);
    }

    #[test]
    fn pure_modifier_during_playback_does_not_count_toward_panic_ring() {
        let h = Harness::armed();
        let cancel = h.app_state.begin_playback().expect("playback starts");
        let modifier = KeyEvent {
            is_pure_modifier: true,
            ..Default::default()
        };
        for _ in 0..3 {
            h.send(&modifier);
        }
        assert!(!cancel.load(Ordering::Relaxed));
    }

    // ---- Esc as an explicit abort -------------------------------------

    #[test]
    fn escape_during_playback_cancels_immediately_and_is_swallowed() {
        let h = Harness::armed();
        let cancel = h.app_state.begin_playback().expect("playback starts");
        assert!(
            matches!(h.send(&esc()), HookDecision::Suppress),
            "Esc must not also reach the target app"
        );
        assert!(cancel.load(Ordering::Relaxed), "one Esc is enough");
        assert_eq!(h.app_state.take_cancel_reason(), Some(CancelReason::Esc));
    }

    #[test]
    fn escape_outside_playback_passes_through() {
        let h = Harness::armed();
        assert!(matches!(h.send(&esc()), HookDecision::Pass));
        assert!(h.app_state.take_cancel_reason().is_none());
    }

    #[test]
    fn escape_while_disarmed_passes_through() {
        let h = Harness::new();
        assert!(matches!(h.send(&esc()), HookDecision::Pass));
    }

    #[test]
    fn panic_ring_still_reports_user_keystrokes() {
        // Esc must not have stolen the ordinary abort's attribution.
        let h = Harness::armed();
        let cancel = h.app_state.begin_playback().expect("playback starts");
        for ch in ['a', 's', 'd'] {
            h.send(&ke(ch));
        }
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(
            h.app_state.take_cancel_reason(),
            Some(CancelReason::UserKeystrokes)
        );
    }

    // ---- Secure-Input gate --------------------------------------------

    #[test]
    fn secure_input_passes_everything_and_never_suppresses() {
        let mut h = Harness::armed();
        h.secure_input_active = true;
        h.matcher.rebuild_index(vec![build_trigger()]);
        for ch in ['b', 'u', 'i', 'l', 'd', '>'] {
            assert!(
                matches!(h.send(&ke(ch)), HookDecision::Pass),
                "nothing may be swallowed near a password field"
            );
        }
        assert_eq!(Harness::n(&h.fires), 0);
        assert_eq!(Harness::n(&h.commits), 0);
    }

    #[test]
    fn secure_input_counts_the_commit_the_user_lost() {
        // Separates "gate closed" (common, harmless) from "gate ate a trigger".
        let mut h = Harness::armed();
        h.secure_input_active = true;
        h.type_str("build>");
        assert_eq!(Harness::n(&h.blocked), 1);
    }

    #[test]
    fn secure_input_does_not_count_ordinary_typing() {
        let mut h = Harness::armed();
        h.secure_input_active = true;
        h.type_str("hunter2");
        assert_eq!(Harness::n(&h.blocked), 0);
    }

    #[test]
    fn secure_input_counts_per_prompt_commit_chars_too() {
        let mut h = Harness::armed();
        h.matcher.rebuild_index(vec![TriggerEntry {
            canonical: "ship".into(),
            prompt_id: "p2".into(),
            word_count: 1,
            commit_char: '!',
        }]);
        h.secure_input_active = true;
        h.type_str("ship!");
        assert_eq!(Harness::n(&h.blocked), 1);
    }

    #[test]
    fn secure_input_gate_is_skipped_when_disarmed() {
        // Disarmed short-circuits first — no phantom "lost trigger" counts.
        let mut h = Harness::new();
        h.secure_input_active = true;
        h.type_str("build>");
        assert_eq!(Harness::n(&h.blocked), 0);
    }

    #[test]
    fn clearing_secure_input_restores_normal_matching() {
        let mut h = Harness::armed();
        h.matcher.rebuild_index(vec![build_trigger()]);
        h.secure_input_active = true;
        h.type_str("build>");
        assert_eq!(Harness::n(&h.fires), 0);

        h.secure_input_active = false;
        h.type_str("build");
        assert!(matches!(h.send(&ke('>')), HookDecision::Suppress));
        assert_eq!(Harness::n(&h.fires), 1);
    }
}
