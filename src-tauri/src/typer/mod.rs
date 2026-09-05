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

/// Raises the OS timer resolution to 1 ms for the lifetime of the guard.
///
/// On Windows, `thread::sleep` honors only the current timer period, which
/// defaults to ~15.6 ms (and is per-process since Win10 2004). Without this,
/// every scheduled IKI gets re-quantized to a multiple of the timer period —
/// re-introducing the exact "everything is a multiple of 16 ms" tell the
/// jitter model (`distributions::jitter`) works to erase, and slowing the
/// fast profiles well below their scheduled cadence. No-op on other platforms,
/// where `thread::sleep` is already sub-millisecond.
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
    /// Insert a line break WITHOUT submitting. In chat apps (the primary
    /// target — ChatGPT, Claude, Slack, Discord) a bare Enter sends the
    /// message, so an embedded `\n` typed as Enter would fire the prompt
    /// mid-body. Shift+Enter is the universal "newline, don't send".
    fn press_shift_enter(&mut self);
    /// Defensive: release any modifier keys that might be physically held.
    /// Called on abort so we don't leave Shift/Ctrl/etc. stuck (§2.7).
    fn release_all_modifiers(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayOutcome {
    pub completed: bool,
    /// Net visible characters delivered so far. Backspaces saturating-subtract
    /// one char so cancellation telemetry reflects partial playback instead
    /// of always reporting all-or-nothing.
    pub visible_chars: usize,
    /// True when playback aborted because the foreground app changed (vs. the
    /// user's panic keystrokes / kill-switch). Lets the caller report a
    /// distinct telemetry reason.
    pub focus_changed: bool,
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
    play_with_progress(schedule, injector, cancel).completed
}

pub fn play_with_progress(
    schedule: &[ScheduledKey],
    injector: &mut dyn Injector,
    cancel: Arc<AtomicBool>,
) -> PlayOutcome {
    play_guarded(schedule, injector, cancel, None)
}

/// Like `play_with_progress`, but also aborts if `focus_lost` returns `true`.
/// `focus_lost` is polled on the same cadence as the cancel flag but throttled
/// to ~100ms so the foreground query stays cheap. When it fires, playback stops
/// and modifiers are released, exactly like a cancel — but the outcome carries
/// `focus_changed: true` so the caller can distinguish the reason.
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
        // An embedded '\n' must NOT be typed as a plain char or a bare Enter
        // (which would submit mid-prompt in a chat app) — it goes through
        // press_shift_enter.
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
