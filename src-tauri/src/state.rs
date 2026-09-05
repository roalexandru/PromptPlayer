//! §10.1 — global app state: armed/disarmed, current playback, panic-stroke ring.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// §2.6 — number of fast keystrokes during playback that abort the expansion.
const PANIC_KEY_COUNT: usize = 3;
/// §2.6 — window the `PANIC_KEY_COUNT` strokes must fit in to count as "fast".
/// Below a normal typing burst, above an accidental two-finger graze.
const PANIC_WINDOW: Duration = Duration::from_millis(600);

/// Global runtime state.
pub struct AppState {
    armed: AtomicBool,
    /// True while a playback (typer thread) is active.
    playing: AtomicBool,
    /// Cancel flag for the current playback (§2.6, §2.7). Minted fresh each
    /// `begin_playback`, so a stale press can't bleed into the next one.
    cancel_flag: Mutex<Arc<AtomicBool>>,
    /// Last `PANIC_KEY_COUNT` keystroke times during playback. All slots full
    /// within `PANIC_WINDOW` means the user panic-aborted (§2.6).
    cancel_strokes: Mutex<[Option<Instant>; PANIC_KEY_COUNT]>,
    /// Why the current playback was cancelled; without it `Esc` and `Kill` were
    /// unreachable reasons. Cleared on every `begin_playback`.
    cancel_reason: Mutex<Option<crate::telemetry::CancelReason>>,
    /// Configurable global commit char (default `>`, §2.3).
    commit_char: Mutex<char>,
    /// The hook is installed and dispatching. False when the macOS tap fails;
    /// the tray surfaces that and a poller respawns once permission lands.
    hook_alive: AtomicBool,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        // §10.1 — app starts disarmed every launch unless the user has opted
        // into restoring it (see `new_armed`).
        Self::new_armed(false)
    }

    /// Explicit initial armed state, for boot when `settings.restore_armed`
    /// is on. Everything else uses [`Self::new`] and the §10.1 default.
    pub fn new_armed(armed: bool) -> Self {
        Self {
            armed: AtomicBool::new(armed),
            playing: AtomicBool::new(false),
            cancel_flag: Mutex::new(Arc::new(AtomicBool::new(false))),
            cancel_strokes: Mutex::new([None; PANIC_KEY_COUNT]),
            cancel_reason: Mutex::new(None),
            commit_char: Mutex::new('>'),
            hook_alive: AtomicBool::new(false),
        }
    }

    pub fn hook_alive(&self) -> bool {
        self.hook_alive.load(Ordering::Relaxed)
    }

    pub fn set_hook_alive(&self, alive: bool) {
        self.hook_alive.store(alive, Ordering::Relaxed);
    }

    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    pub fn shared_armed(armed: bool) -> Arc<Self> {
        Arc::new(Self::new_armed(armed))
    }

    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Relaxed)
    }

    pub fn set_armed(&self, armed: bool) {
        self.armed.store(armed, Ordering::Relaxed);
    }

    pub fn toggle_armed(&self) -> bool {
        let new = !self.is_armed();
        self.set_armed(new);
        new
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Start a playback and return its cancel flag, or `None` if one is already
    /// running. On `None` the caller must NOT call `end_playback`.
    pub fn begin_playback(&self) -> Option<Arc<AtomicBool>> {
        if self
            .playing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let flag = Arc::new(AtomicBool::new(false));
        *self.cancel_flag.lock() = flag.clone();
        *self.cancel_strokes.lock() = [None; PANIC_KEY_COUNT];
        *self.cancel_reason.lock() = None;
        Some(flag)
    }

    pub fn end_playback(&self) {
        self.playing.store(false, Ordering::Release);
        *self.cancel_strokes.lock() = [None; PANIC_KEY_COUNT];
    }

    /// §2.7 kill-switch: trip the cancel flag, recording why.
    pub fn cancel_playback_with(&self, reason: crate::telemetry::CancelReason) {
        // First reason wins — the panic ring can trip while we're already
        // tearing down from an Esc, and the initiating cause is the useful one.
        let mut slot = self.cancel_reason.lock();
        if slot.is_none() {
            *slot = Some(reason);
        }
        drop(slot);
        self.cancel_flag.lock().store(true, Ordering::Relaxed);
    }

    /// §2.7 kill-switch with the default "user typed over it" attribution.
    pub fn cancel_playback(&self) {
        self.cancel_playback_with(crate::telemetry::CancelReason::UserKeystrokes);
    }

    /// Consume the recorded cancel reason, if any.
    pub fn take_cancel_reason(&self) -> Option<crate::telemetry::CancelReason> {
        self.cancel_reason.lock().take()
    }

    /// §2.6 — record a keystroke in the panic ring. True once the ring is full
    /// and every stroke fits inside `PANIC_WINDOW`.
    pub fn record_cancel_keystroke(&self, now: Instant) -> bool {
        let mut ring = self.cancel_strokes.lock();
        // Shift left, append `now` to the last slot.
        for i in 0..PANIC_KEY_COUNT - 1 {
            ring[i] = ring[i + 1];
        }
        ring[PANIC_KEY_COUNT - 1] = Some(now);
        match (ring[0], ring[PANIC_KEY_COUNT - 1]) {
            (Some(oldest), Some(newest)) => newest.duration_since(oldest) <= PANIC_WINDOW,
            _ => false,
        }
    }

    pub fn commit_char(&self) -> char {
        *self.commit_char.lock()
    }

    pub fn set_commit_char(&self, c: char) {
        *self.commit_char.lock() = c;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_disarmed() {
        let s = AppState::new();
        assert!(!s.is_armed(), "§10.1 — app starts disarmed every launch");
    }

    #[test]
    fn toggle_armed_returns_new_state() {
        let s = AppState::new();
        assert!(s.toggle_armed());
        assert!(s.is_armed());
        assert!(!s.toggle_armed());
        assert!(!s.is_armed());
    }

    #[test]
    fn begin_and_end_playback_track_state() {
        let s = AppState::new();
        assert!(!s.is_playing());
        let cancel = s.begin_playback().expect("first playback starts");
        assert!(s.is_playing());
        assert!(!cancel.load(Ordering::Relaxed));
        s.end_playback();
        assert!(!s.is_playing());
    }

    #[test]
    fn begin_playback_is_mutually_exclusive() {
        // A second fire while one is in flight must be refused so keystrokes
        // can't interleave into the same window.
        let s = AppState::new();
        let _first = s.begin_playback().expect("first starts");
        assert!(s.begin_playback().is_none(), "second must be refused");
        s.end_playback();
        assert!(s.begin_playback().is_some(), "starts again after end");
    }

    #[test]
    fn cancel_playback_sets_flag() {
        let s = AppState::new();
        let cancel = s.begin_playback().expect("playback starts");
        s.cancel_playback();
        assert!(cancel.load(Ordering::Relaxed));
        s.end_playback();
        // The next playback gets a FRESH flag (not the cancelled one), so a
        // stale kill-switch press can't bleed into the new session.
        let next = s.begin_playback().expect("playback restarts");
        assert!(!next.load(Ordering::Relaxed));
    }

    #[test]
    fn panic_ring_fires_on_three_fast_keys() {
        // §2.6 — three keystrokes within 600ms cancel the playback.
        let s = AppState::new();
        let _c = s.begin_playback().expect("playback starts");
        let t0 = Instant::now();
        assert!(!s.record_cancel_keystroke(t0));
        assert!(!s.record_cancel_keystroke(t0 + Duration::from_millis(100)));
        assert!(s.record_cancel_keystroke(t0 + Duration::from_millis(300)));
    }

    #[test]
    fn panic_ring_does_not_fire_when_keys_are_spread() {
        let s = AppState::new();
        let _c = s.begin_playback().expect("playback starts");
        let t0 = Instant::now();
        assert!(!s.record_cancel_keystroke(t0));
        // 700ms is wider than the 600ms PANIC_WINDOW, so the oldest entry
        // will be > window away from the newest.
        assert!(!s.record_cancel_keystroke(t0 + Duration::from_millis(400)));
        assert!(!s.record_cancel_keystroke(t0 + Duration::from_millis(800)));
    }

    #[test]
    fn panic_ring_resets_on_begin_playback() {
        let s = AppState::new();
        let _c = s.begin_playback().expect("playback starts");
        let t0 = Instant::now();
        s.record_cancel_keystroke(t0);
        s.record_cancel_keystroke(t0 + Duration::from_millis(50));
        s.end_playback();
        let _c2 = s.begin_playback().expect("playback restarts");
        // Ring is fresh after the new begin; first key should not trigger.
        assert!(!s.record_cancel_keystroke(t0 + Duration::from_millis(60)));
    }

    #[test]
    fn commit_char_default_and_override() {
        let s = AppState::new();
        assert_eq!(s.commit_char(), '>');
        s.set_commit_char('!');
        assert_eq!(s.commit_char(), '!');
    }

    #[test]
    fn new_armed_restores_opt_in_state() {
        // §10.1 stays the default; restore is explicit.
        assert!(!AppState::new().is_armed());
        assert!(AppState::new_armed(true).is_armed());
        assert!(!AppState::new_armed(false).is_armed());
    }

    #[test]
    fn cancel_reason_is_recorded_and_consumed() {
        use crate::telemetry::CancelReason;
        let s = AppState::new();
        let _c = s.begin_playback().expect("playback starts");
        assert!(s.take_cancel_reason().is_none(), "nothing cancelled yet");
        s.cancel_playback_with(CancelReason::Esc);
        assert_eq!(s.take_cancel_reason(), Some(CancelReason::Esc));
        assert!(s.take_cancel_reason().is_none(), "take consumes");
    }

    #[test]
    fn first_cancel_reason_wins() {
        // Esc trips first, then the panic ring as the user keeps hammering.
        use crate::telemetry::CancelReason;
        let s = AppState::new();
        let _c = s.begin_playback().expect("playback starts");
        s.cancel_playback_with(CancelReason::Esc);
        s.cancel_playback_with(CancelReason::UserKeystrokes);
        assert_eq!(s.take_cancel_reason(), Some(CancelReason::Esc));
    }

    #[test]
    fn cancel_reason_resets_between_playbacks() {
        use crate::telemetry::CancelReason;
        let s = AppState::new();
        let _c = s.begin_playback().expect("playback starts");
        s.cancel_playback_with(CancelReason::Kill);
        s.end_playback();
        let _c2 = s.begin_playback().expect("playback restarts");
        assert!(
            s.take_cancel_reason().is_none(),
            "a stale reason must not bleed into the next playback"
        );
    }

    #[test]
    fn plain_cancel_defaults_to_user_keystrokes() {
        use crate::telemetry::CancelReason;
        let s = AppState::new();
        let _c = s.begin_playback().expect("playback starts");
        s.cancel_playback();
        assert_eq!(s.take_cancel_reason(), Some(CancelReason::UserKeystrokes));
    }

    #[test]
    fn shared_returns_arc() {
        let a = AppState::shared();
        let b = a.clone();
        a.set_armed(true);
        // Both Arc clones see the same state.
        assert!(b.is_armed());
    }
}
