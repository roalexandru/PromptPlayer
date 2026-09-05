//! §9.1 — macOS Secure Event Input stops a `CGEventTap` suppressing keystrokes
//! near a password field, so we pass everything through and count what's lost.
//!
//! [`SecureInputTracker`] aggregates rather than reporting edges: per-edge
//! events were 91% of all telemetry and said nothing useful.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
extern "C" {
    #[allow(dead_code)]
    fn IsSecureEventInputEnabled() -> u8;
}

/// True when Secure Event Input is engaged; false off macOS and in tests. The
/// hook calls it per keystroke — a stale value means touching a password field.
#[allow(unused)]
pub fn is_active() -> bool {
    #[cfg(test)]
    {
        false
    }
    #[cfg(all(target_os = "macos", not(test)))]
    unsafe {
        IsSecureEventInputEnabled() != 0
    }
    #[cfg(all(not(target_os = "macos"), not(test)))]
    {
        false
    }
}

/// One window's worth of Secure-Input activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecureInputStats {
    /// Rising edges observed during the window.
    pub activations: u32,
    /// Total time the gate spent closed during the window.
    pub active: Duration,
    /// Commit chars typed while closed — each a trigger that did nothing.
    pub blocked_commits: u32,
}

impl SecureInputStats {
    pub fn is_empty(&self) -> bool {
        self.activations == 0 && self.blocked_commits == 0 && self.active.is_zero()
    }
}

/// Shared by the poller (`observe`), the hook (`note_blocked_commit`) and the
/// UI (`is_active_cached`).
#[derive(Debug, Default)]
pub struct SecureInputTracker {
    /// Last polled state. UI only — never used to gate keys.
    active: AtomicBool,
    activations: AtomicU32,
    /// Completed closed-intervals, in milliseconds.
    active_ms: AtomicU64,
    blocked_commits: AtomicU32,
    /// When the currently-open interval began, if the gate is closed now.
    since: Mutex<Option<Instant>>,
}

impl SecureInputTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Last polled state. For display only — see [`is_active`].
    pub fn is_active_cached(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// A commit char was typed while the gate was closed. Separates "secure
    /// input happens a lot" (harmless) from "it is eating triggers".
    pub fn note_blocked_commit(&self) {
        self.blocked_commits.fetch_add(1, Ordering::Relaxed);
    }

    /// Feed the poller's current reading. Only transitions do work.
    pub fn observe(&self, now_active: bool) {
        self.observe_at(now_active, Instant::now())
    }

    fn observe_at(&self, now_active: bool, at: Instant) {
        let mut since = self.since.lock();
        let was_active = since.is_some();
        if now_active == was_active {
            return;
        }
        if now_active {
            *since = Some(at);
            self.activations.fetch_add(1, Ordering::Relaxed);
        } else if let Some(start) = since.take() {
            let ms = at.saturating_duration_since(start).as_millis() as u64;
            self.active_ms.fetch_add(ms, Ordering::Relaxed);
        }
        self.active.store(now_active, Ordering::Relaxed);
    }

    /// Take everything accumulated and reset. An open interval counts so far and
    /// restarts, so a gate closed all window doesn't report zero.
    pub fn drain(&self) -> SecureInputStats {
        self.drain_at(Instant::now())
    }

    fn drain_at(&self, at: Instant) -> SecureInputStats {
        let mut since = self.since.lock();
        if let Some(start) = since.as_mut() {
            let ms = at.saturating_duration_since(*start).as_millis() as u64;
            self.active_ms.fetch_add(ms, Ordering::Relaxed);
            *start = at;
        }
        SecureInputStats {
            activations: self.activations.swap(0, Ordering::Relaxed),
            active: Duration::from_millis(self.active_ms.swap(0, Ordering::Relaxed)),
            blocked_commits: self.blocked_commits.swap(0, Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_active_returns_false_in_tests() {
        // Sanity check: tests must not be sensitive to the dev's local
        // SecureInput state. The cfg(test) override above guarantees this.
        assert!(!is_active());
    }

    #[test]
    fn drain_of_a_quiet_window_is_empty() {
        let t = SecureInputTracker::new();
        assert!(t.drain().is_empty(), "a quiet window must report nothing");
        assert!(!t.is_active_cached());
    }

    #[test]
    fn repeated_same_state_polls_do_not_count() {
        // The poller runs every 2s; a gate closed for an hour must report one
        // activation, not 1,800.
        let t = SecureInputTracker::new();
        let t0 = Instant::now();
        t.observe_at(true, t0);
        for i in 1..50 {
            t.observe_at(true, t0 + Duration::from_secs(2 * i));
        }
        let s = t.drain_at(t0 + Duration::from_secs(100));
        assert_eq!(s.activations, 1, "one close is one activation");
        assert_eq!(s.active, Duration::from_secs(100));
    }

    #[test]
    fn counts_each_close_and_sums_durations() {
        let t = SecureInputTracker::new();
        let t0 = Instant::now();
        t.observe_at(true, t0);
        t.observe_at(false, t0 + Duration::from_secs(4));
        t.observe_at(true, t0 + Duration::from_secs(10));
        t.observe_at(false, t0 + Duration::from_secs(16));
        let s = t.drain_at(t0 + Duration::from_secs(20));
        assert_eq!(s.activations, 2);
        assert_eq!(s.active, Duration::from_secs(10));
        assert!(!s.is_empty());
    }

    #[test]
    fn drain_includes_and_restarts_an_open_interval() {
        let t = SecureInputTracker::new();
        let t0 = Instant::now();
        t.observe_at(true, t0);
        let first = t.drain_at(t0 + Duration::from_secs(30));
        assert_eq!(first.active, Duration::from_secs(30));
        // Still closed: the next window counts from the drain, not the edge.
        let second = t.drain_at(t0 + Duration::from_secs(50));
        assert_eq!(second.active, Duration::from_secs(20));
        assert_eq!(second.activations, 0, "no new edge, no new activation");
    }

    #[test]
    fn drain_resets_counters() {
        let t = SecureInputTracker::new();
        let t0 = Instant::now();
        t.observe_at(true, t0);
        t.note_blocked_commit();
        t.observe_at(false, t0 + Duration::from_secs(1));
        let first = t.drain_at(t0 + Duration::from_secs(2));
        assert_eq!(first.activations, 1);
        assert_eq!(first.blocked_commits, 1);
        assert!(t.drain_at(t0 + Duration::from_secs(3)).is_empty());
    }

    #[test]
    fn blocked_commits_accumulate() {
        let t = SecureInputTracker::new();
        t.note_blocked_commit();
        t.note_blocked_commit();
        t.note_blocked_commit();
        assert_eq!(t.drain().blocked_commits, 3);
    }

    #[test]
    fn cached_flag_tracks_the_last_poll() {
        let t = SecureInputTracker::new();
        assert!(!t.is_active_cached());
        t.observe(true);
        assert!(t.is_active_cached());
        t.observe(false);
        assert!(!t.is_active_cached());
    }
}
