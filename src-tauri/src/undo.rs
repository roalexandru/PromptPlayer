//! §2.4 — backspace-undo for misfired expansions.
//!
//! On expansion: record `{trigger_word, body_typed_chars, fired_at}` in a 2s ring.
//! On Backspace within 2s of an expansion: send `body.len()` backspaces and
//! retype the original trigger.
//!
//! Espanso ships this; it's critical demo recovery (§2.4).

use parking_lot::Mutex;
use std::time::{Duration, Instant};

pub const UNDO_WINDOW: Duration = Duration::from_secs(2);

/// One recorded expansion that can be undone.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub trigger_form: String,
    /// Total characters that were typed into the target app as part of the body.
    /// Excludes backspaces issued during typo corrections (those net out).
    pub body_chars_typed: usize,
    pub fired_at: Instant,
}

#[derive(Debug, Default)]
pub struct UndoLog {
    entries: Mutex<Vec<UndoEntry>>,
}

impl UndoLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, trigger_form: String, body_chars_typed: usize) {
        let mut e = self.entries.lock();
        // Trim stale entries (older than the window).
        let now = Instant::now();
        e.retain(|en| now.duration_since(en.fired_at) <= UNDO_WINDOW);
        e.push(UndoEntry {
            trigger_form,
            body_chars_typed,
            fired_at: now,
        });
    }

    /// Pop the most recent entry that's still within the undo window.
    /// Returns None if no recent entry exists.
    pub fn take_recent(&self, now: Instant) -> Option<UndoEntry> {
        let mut e = self.entries.lock();
        // Drop stale.
        e.retain(|en| now.duration_since(en.fired_at) <= UNDO_WINDOW);
        e.pop()
    }

    pub fn clear(&self) {
        self.entries.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn record_and_take_within_window() {
        let log = UndoLog::new();
        log.record("Build".into(), 88);
        let now = Instant::now();
        let e = log.take_recent(now).unwrap();
        assert_eq!(e.trigger_form, "Build");
        assert_eq!(e.body_chars_typed, 88);
    }

    #[test]
    fn empty_after_window() {
        let log = UndoLog::new();
        log.record("Build".into(), 10);
        sleep(Duration::from_millis(50));
        // Force the entry to look stale by passing a "now" far in the future.
        let future = Instant::now() + Duration::from_secs(5);
        assert!(log.take_recent(future).is_none());
    }

    #[test]
    fn lifo_for_overlapping() {
        let log = UndoLog::new();
        log.record("First".into(), 10);
        log.record("Second".into(), 20);
        let now = Instant::now();
        let e = log.take_recent(now).unwrap();
        assert_eq!(e.trigger_form, "Second");
    }
}
