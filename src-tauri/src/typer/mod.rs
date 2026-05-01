//! §3 — typing engine.
//!
//! The schedule (`schedule.rs`) is computed up front; the Typer (`mod.rs`)
//! sleeps to each absolute time and asks the Injector to fire the keystroke.
//! Cancellation flag is checked between every key.
//! On cancel, the Typer releases all modifiers (§2.7 defensive).

pub mod distributions;
pub mod profiles;
pub mod schedule;
pub mod typos;

pub use profiles::{Profile, ProfileKind, TypingOverrides};
pub use schedule::{schedule, Key, ScheduleOptions, ScheduledKey};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Trait abstracting keystroke synthesis. Implemented per platform.
///
/// Intentionally NOT `Send`: the macOS `Enigo` impl holds a `CGEventSource`
/// raw pointer that can't cross threads. The Typer pattern is "create the
/// injector on the typer thread, then play() the pre-computed schedule" —
/// the schedule itself is `Send`, the injector is constructed thread-local.
pub trait Injector {
    fn type_char(&mut self, c: char);
    fn press_backspace(&mut self);
    fn press_enter(&mut self);
    /// Defensive: release any modifier keys that might be physically held.
    /// Called on abort so we don't leave Shift/Ctrl/etc. stuck (§2.7).
    fn release_all_modifiers(&mut self);
}

/// Plays a pre-computed schedule. Returns `true` on full completion, `false` if cancelled.
///
/// `cancel` is checked before every key. When set, the Typer:
/// 1. Stops issuing keystrokes.
/// 2. Calls `injector.release_all_modifiers()` (§2.7).
pub fn play(
    schedule: &[ScheduledKey],
    injector: &mut dyn Injector,
    cancel: Arc<AtomicBool>,
) -> bool {
    let start = Instant::now();
    for sk in schedule {
        if cancel.load(Ordering::Relaxed) {
            injector.release_all_modifiers();
            return false;
        }
        let target = Duration::from_millis(sk.absolute_time_ms);
        let elapsed = start.elapsed();
        if target > elapsed {
            // Spin-sleep in chunks to keep cancel responsive (max 50ms).
            let mut remaining = target - elapsed;
            while remaining > Duration::from_millis(50) {
                if cancel.load(Ordering::Relaxed) {
                    injector.release_all_modifiers();
                    return false;
                }
                thread::sleep(Duration::from_millis(50));
                let new_elapsed = start.elapsed();
                if new_elapsed >= target {
                    remaining = Duration::ZERO;
                    break;
                }
                remaining = target - new_elapsed;
            }
            if remaining > Duration::ZERO {
                thread::sleep(remaining);
            }
        }
        match sk.key {
            Key::Char(c) => injector.type_char(c),
            Key::Backspace => injector.press_backspace(),
            Key::Enter => injector.press_enter(),
        }
    }
    true
}

/// In-process injector that just records the calls. Used in tests and the
/// `--dry-run` mode of the CLI.
pub struct RecordingInjector {
    pub events: Vec<Key>,
    pub modifier_releases: usize,
}

impl Default for RecordingInjector {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            modifier_releases: 0,
        }
    }
}

impl Injector for RecordingInjector {
    fn type_char(&mut self, c: char) {
        self.events.push(Key::Char(c));
    }
    fn press_backspace(&mut self) {
        self.events.push(Key::Backspace);
    }
    fn press_enter(&mut self) {
        self.events.push(Key::Enter);
    }
    fn release_all_modifiers(&mut self) {
        self.modifier_releases += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn play_completes_short_schedule() {
        let text = "Hi there";
        let profile = Profile { typos_enabled: false, pre_submit_pause_enabled: false, ..Profile::SALES_ENGINEER };
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        // Disable pre-typing pause so the test is fast.
        let opts = ScheduleOptions { rdp_mode: false, include_pre_typing_pause: false };
        let s = schedule(text, &profile, &opts, &mut rng);
        let mut inj = RecordingInjector::default();
        let cancel = Arc::new(AtomicBool::new(false));
        // Truncate to quick playback by clamping all times to <50ms.
        let fast: Vec<_> = s
            .iter()
            .map(|k| ScheduledKey { absolute_time_ms: 0, ..*k })
            .collect();
        let ok = play(&fast, &mut inj, cancel);
        assert!(ok);
        let chars: String = inj.events.iter().filter_map(|e| match e {
            Key::Char(c) => Some(*c),
            _ => None,
        }).collect();
        assert_eq!(chars, text);
    }

    #[test]
    fn cancel_releases_modifiers() {
        let text = "abcdefghij";
        let profile = Profile { typos_enabled: false, ..Profile::SALES_ENGINEER };
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let opts = ScheduleOptions { rdp_mode: false, include_pre_typing_pause: false };
        let mut s = schedule(text, &profile, &opts, &mut rng);
        // Push out times to give cancel a chance to land.
        for (i, k) in s.iter_mut().enumerate() {
            k.absolute_time_ms = (i as u64) * 200;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel.clone();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            cancel_clone.store(true, Ordering::Relaxed);
        });
        let mut inj = RecordingInjector::default();
        let ok = play(&s, &mut inj, cancel);
        handle.join().unwrap();
        assert!(!ok);
        assert!(inj.modifier_releases >= 1, "release_all_modifiers must run on cancel");
    }
}
