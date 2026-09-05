//! §10.1 — global app state: armed/disarmed, current playback, panic-stroke ring.

use crate::typer::PlaybackControl;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// §2.6 — number of fast keystrokes during playback that abort the expansion.
const PANIC_KEY_COUNT: usize = 3;
/// §2.6 — sliding window the `PANIC_KEY_COUNT` strokes must fit inside to count
/// as "fast". 600 ms is comfortably below a normal typing burst yet well above
/// an accidental two-finger graze.
const PANIC_WINDOW: Duration = Duration::from_millis(600);

/// Global runtime state.
pub struct AppState {
    armed: AtomicBool,
    /// True while a playback (typer thread) is active.
    playing: AtomicBool,
    /// Controls for the *current* playback (§2.6, §2.7, plus pause/speed). A
    /// fresh handle is minted on each `begin_playback` so a stale kill-switch
    /// press from a previous playback can't bleed into the next one, and so
    /// `end_playback` of one session can never clear another's flag.
    playback: Mutex<PlaybackControl>,
    /// Ring of the last `PANIC_KEY_COUNT` keystroke timestamps observed during
    /// playback. When all slots are filled and `newest - oldest <= PANIC_WINDOW`,
    /// the user has panic-aborted (§2.6).
    cancel_strokes: Mutex<[Option<Instant>; PANIC_KEY_COUNT]>,
    /// Configurable global commit char (default `>`, §2.3).
    commit_char: Mutex<char>,
    /// When the app was last armed. Drives the §11 auto-disarm timer; `None`
    /// whenever disarmed.
    armed_since: Mutex<Option<Instant>>,
    /// Cursor into the configured setlist — index of the *next* cue to fire.
    setlist_cursor: Mutex<usize>,
    /// True while the platform keyboard hook is installed and dispatching events.
    /// On macOS this flips false when `CGEventTapCreate` fails (Accessibility
    /// missing) or the run loop exits. The frontend reads this to surface a
    /// "grant Accessibility" row in the tray; an Accessibility-status poller
    /// respawns the hook when permission is granted.
    hook_alive: AtomicBool,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            // §10.1 — app starts disarmed every launch.
            armed: AtomicBool::new(false),
            playing: AtomicBool::new(false),
            playback: Mutex::new(PlaybackControl::new()),
            cancel_strokes: Mutex::new([None; PANIC_KEY_COUNT]),
            commit_char: Mutex::new('>'),
            armed_since: Mutex::new(None),
            setlist_cursor: Mutex::new(0),
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

    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Relaxed)
    }

    pub fn set_armed(&self, armed: bool) {
        self.armed.store(armed, Ordering::Relaxed);
        // Restart the auto-disarm clock on every transition into armed, and
        // clear it on the way out so a disarmed app is never "overdue".
        *self.armed_since.lock() = armed.then(Instant::now);
    }

    /// How long the app has been armed, or `None` when disarmed.
    pub fn armed_for(&self) -> Option<Duration> {
        self.armed_since.lock().map(|t| t.elapsed())
    }

    /// Index of the next setlist cue to fire.
    pub fn setlist_cursor(&self) -> usize {
        *self.setlist_cursor.lock()
    }

    pub fn set_setlist_cursor(&self, index: usize) {
        *self.setlist_cursor.lock() = index;
    }

    /// Advance the setlist cursor, wrapping at `len`. Returns the index that
    /// should fire now (i.e. the pre-advance value). `None` for an empty list.
    pub fn take_next_cue(&self, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        let mut cur = self.setlist_cursor.lock();
        let index = *cur % len;
        *cur = (index + 1) % len;
        Some(index)
    }

    pub fn toggle_armed(&self) -> bool {
        let new = !self.is_armed();
        self.set_armed(new);
        new
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Try to start a playback. Returns the cancel flag the typer thread should
    /// poll, or `None` if a playback is already in progress — playbacks are
    /// mutually exclusive so two fires can't interleave keystrokes into the
    /// same window. The caller MUST NOT call `end_playback` when this returns
    /// `None` (it would flip `playing` false out from under the live session).
    pub fn begin_playback(&self) -> Option<PlaybackControl> {
        if self
            .playing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        // A fresh control (not a reset of the old one): pause state and speed
        // must not carry over from the previous prompt either.
        let control = PlaybackControl::new();
        *self.playback.lock() = control.clone();
        *self.cancel_strokes.lock() = [None; PANIC_KEY_COUNT];
        Some(control)
    }

    /// Handle for the playback in flight. Hotkey handlers use this to pause or
    /// re-speed the run without going through the fire pipeline.
    pub fn playback_control(&self) -> PlaybackControl {
        self.playback.lock().clone()
    }

    /// §3.5 — pause / resume the running playback. Returns the new paused
    /// state, or `None` when nothing is playing.
    pub fn toggle_pause(&self) -> Option<bool> {
        if !self.is_playing() {
            return None;
        }
        Some(self.playback.lock().toggle_paused())
    }

    /// Multiply the running playback's speed. Returns the new multiplier, or
    /// `None` when nothing is playing.
    pub fn nudge_speed(&self, factor: f64) -> Option<f64> {
        if !self.is_playing() {
            return None;
        }
        Some(self.playback.lock().nudge_speed(factor))
    }

    pub fn end_playback(&self) {
        self.playing.store(false, Ordering::Release);
        *self.cancel_strokes.lock() = [None; PANIC_KEY_COUNT];
        // Leave no pause latched: the next fire must start moving immediately.
        self.playback.lock().reset();
    }

    /// §2.7 kill-switch: cancel the current playback. Clears any pause first —
    /// a paused typer is parked in a poll loop and has to observe the cancel.
    pub fn cancel_playback(&self) {
        let control = self.playback.lock().clone();
        control.set_paused(false);
        control.cancel();
    }

    /// §2.6 — record a keystroke at `now` in the panic ring. Returns `true` iff
    /// the ring is full and all `PANIC_KEY_COUNT` strokes fit inside
    /// `PANIC_WINDOW` (i.e. the user has panic-aborted).
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
        assert!(!cancel.is_cancelled());
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
        assert!(cancel.is_cancelled());
        s.end_playback();
        // The next playback gets a FRESH flag (not the cancelled one), so a
        // stale kill-switch press can't bleed into the new session.
        let next = s.begin_playback().expect("playback restarts");
        assert!(!next.is_cancelled());
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
    fn armed_for_tracks_the_arm_transition() {
        let s = AppState::new();
        assert!(s.armed_for().is_none(), "disarmed has no clock");
        s.set_armed(true);
        assert!(s.armed_for().is_some());
        s.set_armed(false);
        assert!(
            s.armed_for().is_none(),
            "disarming must clear the clock, or auto-disarm would fire instantly on re-arm"
        );
    }

    #[test]
    fn re_arming_restarts_the_auto_disarm_clock() {
        let s = AppState::new();
        s.set_armed(true);
        std::thread::sleep(Duration::from_millis(20));
        let first = s.armed_for().unwrap();
        s.set_armed(false);
        s.set_armed(true);
        let second = s.armed_for().unwrap();
        assert!(
            second < first,
            "{second:?} should be fresher than {first:?}"
        );
    }

    #[test]
    fn take_next_cue_advances_and_wraps() {
        let s = AppState::new();
        assert_eq!(s.take_next_cue(3), Some(0));
        assert_eq!(s.take_next_cue(3), Some(1));
        assert_eq!(s.take_next_cue(3), Some(2));
        assert_eq!(s.take_next_cue(3), Some(0), "wraps back to the top");
    }

    #[test]
    fn take_next_cue_on_an_empty_setlist_is_none() {
        let s = AppState::new();
        assert_eq!(s.take_next_cue(0), None);
    }

    #[test]
    fn take_next_cue_handles_a_shrunken_setlist() {
        // The cursor persists across edits; a list that got shorter must not
        // index out of bounds.
        let s = AppState::new();
        s.set_setlist_cursor(7);
        assert_eq!(s.take_next_cue(2), Some(1), "7 % 2");
        assert_eq!(s.setlist_cursor(), 0);
    }

    #[test]
    fn pause_and_speed_are_no_ops_when_nothing_is_playing() {
        let s = AppState::new();
        assert!(s.toggle_pause().is_none());
        assert!(s.nudge_speed(2.0).is_none());
    }

    #[test]
    fn pause_toggles_the_live_playback() {
        let s = AppState::new();
        let control = s.begin_playback().unwrap();
        assert!(!control.is_paused());
        assert_eq!(s.toggle_pause(), Some(true));
        assert!(control.is_paused(), "the typer thread sees the flag");
        assert_eq!(s.toggle_pause(), Some(false));
        assert!(!control.is_paused());
    }

    #[test]
    fn speed_nudges_are_clamped() {
        let s = AppState::new();
        let _c = s.begin_playback().unwrap();
        for _ in 0..20 {
            s.nudge_speed(2.0);
        }
        assert_eq!(s.nudge_speed(1.0), Some(4.0), "clamped at the top");
        for _ in 0..40 {
            s.nudge_speed(0.5);
        }
        assert_eq!(s.nudge_speed(1.0), Some(0.25), "clamped at the bottom");
    }

    #[test]
    fn cancel_clears_a_pause_so_the_parked_typer_can_observe_it() {
        // A paused typer sits in a poll loop; if cancel left the pause latched
        // the kill switch would never take effect.
        let s = AppState::new();
        let control = s.begin_playback().unwrap();
        s.toggle_pause();
        assert!(control.is_paused());
        s.cancel_playback();
        assert!(!control.is_paused());
        assert!(control.is_cancelled());
    }

    #[test]
    fn end_playback_leaves_no_pause_or_speed_latched() {
        let s = AppState::new();
        let first = s.begin_playback().unwrap();
        s.toggle_pause();
        s.nudge_speed(2.0);
        s.end_playback();
        let next = s.begin_playback().unwrap();
        assert!(!next.is_paused(), "a fresh fire must start moving");
        assert_eq!(next.speed(), 1.0);
        // And the old handle is genuinely a different one.
        assert!(!first.is_paused());
    }

    #[test]
    fn commit_char_default_and_override() {
        let s = AppState::new();
        assert_eq!(s.commit_char(), '>');
        s.set_commit_char('!');
        assert_eq!(s.commit_char(), '!');
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
