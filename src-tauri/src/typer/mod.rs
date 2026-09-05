//! §3 — typing engine. Sleeps to each absolute time in the pre-computed
//! schedule, checking the cancel flag between keys and releasing modifiers
//! on abort (§2.7).

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

/// Raise the OS timer resolution to 1 ms while held. Windows' ~15.6ms default
/// would re-quantize every IKI to a multiple of 16 ms. No-op elsewhere.
pub struct TimerResolutionGuard {
    #[cfg(target_os = "windows")]
    active: bool,
}

impl TimerResolutionGuard {
    pub fn acquire() -> Self {
        #[cfg(target_os = "windows")]
        {
            // MMRESULT == TIMERR_NOERROR (0) means the period was accepted.
            let active = unsafe { windows::Win32::Media::timeBeginPeriod(1) } == 0;
            if !active {
                tracing::warn!("timeBeginPeriod(1) failed; playback cadence may quantize");
            }
            Self { active }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Self {}
        }
    }
}

impl Drop for TimerResolutionGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        if self.active {
            unsafe {
                let _ = windows::Win32::Media::timeEndPeriod(1);
            }
        }
    }
}

/// Per-platform keystroke synthesis, deliberately not `Send` — injectors are
/// built on the typer thread while only the schedule crosses threads.
pub trait Injector {
    fn type_char(&mut self, c: char);
    fn press_backspace(&mut self);
    fn press_enter(&mut self);
    /// Line break without submitting. A bare Enter sends the message in every
    /// chat app, so an embedded `\n` would fire the prompt mid-body.
    fn press_shift_enter(&mut self);
    /// Defensive: release any modifier keys that might be physically held.
    /// Called on abort so we don't leave Shift/Ctrl/etc. stuck (§2.7).
    fn release_all_modifiers(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayOutcome {
    pub completed: bool,
    /// Net visible chars so far; backspaces saturating-subtract, so cancel
    /// telemetry reports partial progress rather than all-or-nothing.
    pub visible_chars: usize,
    /// Aborted by a foreground change rather than the user's keystrokes, so the
    /// caller can report a distinct reason.
    pub focus_changed: bool,
}

/// Play a pre-computed schedule; false if cancelled. `cancel` is checked before
/// every key, and a set flag stops typing and releases modifiers (§2.7).
pub fn play(
    schedule: &[ScheduledKey],
    injector: &mut dyn Injector,
    cancel: Arc<AtomicBool>,
) -> bool {
    play_with_progress(schedule, injector, cancel).completed
}

pub fn play_with_progress(
    schedule: &[ScheduledKey],
    injector: &mut dyn Injector,
    cancel: Arc<AtomicBool>,
) -> PlayOutcome {
    play_guarded(schedule, injector, cancel, None)
}

/// `play_with_progress` plus a `focus_lost` abort, polled with the cancel flag
/// but throttled to ~100ms. Behaves like a cancel, but flags `focus_changed`.
pub fn play_guarded(
    schedule: &[ScheduledKey],
    injector: &mut dyn Injector,
    cancel: Arc<AtomicBool>,
    focus_lost: Option<&dyn Fn() -> bool>,
) -> PlayOutcome {
    let start = Instant::now();
    let mut visible_chars = 0usize;
    let focus_throttle = Duration::from_millis(100);
    let mut last_focus_check = Instant::now();
    // Returns Some(outcome) if we should stop now (cancel or focus change).
    let should_stop = |visible_chars: usize, last: &mut Instant| -> Option<PlayOutcome> {
        if cancel.load(Ordering::Relaxed) {
            return Some(PlayOutcome {
                completed: false,
                visible_chars,
                focus_changed: false,
            });
        }
        if let Some(check) = focus_lost {
            if last.elapsed() >= focus_throttle {
                *last = Instant::now();
                if check() {
                    return Some(PlayOutcome {
                        completed: false,
                        visible_chars,
                        focus_changed: true,
                    });
                }
            }
        }
        None
    };
    for sk in schedule {
        if let Some(outcome) = should_stop(visible_chars, &mut last_focus_check) {
            injector.release_all_modifiers();
            return outcome;
        }
        let target = Duration::from_millis(sk.absolute_time_ms);
        let elapsed = start.elapsed();
        if target > elapsed {
            // Spin-sleep in chunks to keep cancel/focus checks responsive (max 50ms).
            let mut remaining = target - elapsed;
            while remaining > Duration::from_millis(50) {
                if let Some(outcome) = should_stop(visible_chars, &mut last_focus_check) {
                    injector.release_all_modifiers();
                    return outcome;
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
            // Embedded newlines are inserted as Shift+Enter so multi-paragraph
            // prompts don't submit mid-body in chat apps.
            Key::Char('\n') => {
                injector.press_shift_enter();
                visible_chars += 1;
            }
            Key::Char(c) => {
                injector.type_char(c);
                visible_chars += 1;
            }
            Key::Backspace => {
                injector.press_backspace();
                visible_chars = visible_chars.saturating_sub(1);
            }
            Key::Enter => injector.press_enter(),
        }
    }
    PlayOutcome {
        completed: true,
        visible_chars,
        focus_changed: false,
    }
}

/// In-process injector that just records the calls. Used in tests and the
/// `--dry-run` mode of the CLI.
#[derive(Default)]
pub struct RecordingInjector {
    pub events: Vec<Key>,
    pub modifier_releases: usize,
    pub shift_enters: usize,
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
    fn press_shift_enter(&mut self) {
        // Record as a literal newline char so reconstructed text round-trips.
        self.events.push(Key::Char('\n'));
        self.shift_enters += 1;
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
        let profile = Profile {
            typos_enabled: false,
            pre_submit_pause_enabled: false,
            ..Profile::SALES_ENGINEER
        };
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        // Disable pre-typing pause so the test is fast.
        let opts = ScheduleOptions {
            rdp_mode: false,
            include_pre_typing_pause: false,
        };
        let s = schedule(text, &profile, &opts, &mut rng);
        let mut inj = RecordingInjector::default();
        let cancel = Arc::new(AtomicBool::new(false));
        // Truncate to quick playback by clamping all times to <50ms.
        let fast: Vec<_> = s
            .iter()
            .map(|k| ScheduledKey {
                absolute_time_ms: 0,
                ..*k
            })
            .collect();
        let ok = play(&fast, &mut inj, cancel);
        assert!(ok);
        let chars: String = inj
            .events
            .iter()
            .filter_map(|e| match e {
                Key::Char(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(chars, text);
    }

    #[test]
    fn cancel_releases_modifiers() {
        let text = "abcdefghij";
        let profile = Profile {
            typos_enabled: false,
            ..Profile::SALES_ENGINEER
        };
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let opts = ScheduleOptions {
            rdp_mode: false,
            include_pre_typing_pause: false,
        };
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
        assert!(
            inj.modifier_releases >= 1,
            "release_all_modifiers must run on cancel"
        );
    }

    #[test]
    fn embedded_newline_routes_to_shift_enter() {
        // Never a plain char or bare Enter — that submits mid-prompt in a chat
        // app. Goes through `press_shift_enter`.
        let schedule = vec![
            ScheduledKey {
                key: Key::Char('a'),
                absolute_time_ms: 0,
                is_correction: false,
                is_burst: false,
            },
            ScheduledKey {
                key: Key::Char('\n'),
                absolute_time_ms: 0,
                is_correction: false,
                is_burst: false,
            },
            ScheduledKey {
                key: Key::Char('b'),
                absolute_time_ms: 0,
                is_correction: false,
                is_burst: false,
            },
        ];
        let mut inj = RecordingInjector::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let out = play_with_progress(&schedule, &mut inj, cancel);
        assert!(out.completed);
        assert_eq!(inj.shift_enters, 1, "newline must use shift+enter");
        assert!(
            !inj.events.iter().any(|e| matches!(e, Key::Enter)),
            "must not emit a bare Enter for an embedded newline"
        );
    }

    #[test]
    fn focus_change_aborts_playback() {
        // A focus change mid-playback must stop typing and flag the reason,
        // so the remainder doesn't land in the wrong window.
        let schedule: Vec<ScheduledKey> = "abcdefghij"
            .chars()
            .enumerate()
            .map(|(i, c)| ScheduledKey {
                key: Key::Char(c),
                absolute_time_ms: (i as u64) * 150,
                is_correction: false,
                is_burst: false,
            })
            .collect();
        let cancel = Arc::new(AtomicBool::new(false));
        // Report "focus lost" once the clock has advanced past the throttle.
        let start = Instant::now();
        let lost = move || start.elapsed() >= Duration::from_millis(120);
        let mut inj = RecordingInjector::default();
        let out = play_guarded(&schedule, &mut inj, cancel, Some(&lost));
        assert!(!out.completed);
        assert!(out.focus_changed, "abort reason must be focus change");
        assert!(
            inj.modifier_releases >= 1,
            "modifiers must be released on focus-change abort"
        );
        assert!(
            inj.events.len() < 10,
            "playback should have stopped early, not typed everything"
        );
    }

    #[test]
    fn play_with_progress_reports_partial_visible_chars() {
        let schedule = vec![
            ScheduledKey {
                key: Key::Char('a'),
                absolute_time_ms: 0,
                is_correction: false,
                is_burst: false,
            },
            ScheduledKey {
                key: Key::Char('b'),
                absolute_time_ms: 0,
                is_correction: false,
                is_burst: false,
            },
            ScheduledKey {
                key: Key::Backspace,
                absolute_time_ms: 0,
                is_correction: true,
                is_burst: false,
            },
        ];
        let mut inj = RecordingInjector::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let out = play_with_progress(&schedule, &mut inj, cancel);
        assert!(out.completed);
        assert_eq!(out.visible_chars, 1);
    }
}
