//! §10.1 — global app state: armed/disarmed, current playback, panic-stroke ring.

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
    /// Cancellation flag the typer thread polls (§2.6, §2.7).
    cancel_flag: Arc<AtomicBool>,
    /// Ring of the last `PANIC_KEY_COUNT` keystroke timestamps observed during
    /// playback. When all slots are filled and `newest - oldest <= PANIC_WINDOW`,
    /// the user has panic-aborted (§2.6).
    cancel_strokes: Mutex<[Option<Instant>; PANIC_KEY_COUNT]>,
    /// Configurable global commit char (default `>`, §2.3).
    commit_char: Mutex<char>,
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
            cancel_flag: Arc::new(AtomicBool::new(false)),
            cancel_strokes: Mutex::new([None; PANIC_KEY_COUNT]),
            commit_char: Mutex::new('>'),
        }
    }

    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
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

    pub fn begin_playback(&self) -> Arc<AtomicBool> {
        self.playing.store(true, Ordering::Relaxed);
        self.cancel_flag.store(false, Ordering::Relaxed);
        *self.cancel_strokes.lock() = [None; PANIC_KEY_COUNT];
        self.cancel_flag.clone()
    }

    pub fn end_playback(&self) {
        self.playing.store(false, Ordering::Relaxed);
        self.cancel_flag.store(false, Ordering::Relaxed);
        *self.cancel_strokes.lock() = [None; PANIC_KEY_COUNT];
    }

    /// §2.7 kill-switch: set the cancel flag.
    pub fn cancel_playback(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
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
