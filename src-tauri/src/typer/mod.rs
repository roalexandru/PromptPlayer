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
pub use schedule::{newline_keys, schedule, Key, ScheduleOptions, ScheduledKey};

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Live controls for the playback in flight: cancel, pause, and speed.
///
/// §3.5 only had "abort". Mid-demo the useful gesture is usually *pause* —
/// stop typing, narrate what is on screen, then resume where you left off.
/// Killing the playback throws the rest of the prompt away, so the kill switch
/// can't serve that purpose.
///
/// Cheap to clone; every field is `Arc`-shared with the typer thread.
#[derive(Clone)]
pub struct PlaybackControl {
    /// §2.6 / §2.7 — hard abort. Kept public because it predates this struct
    /// and several call sites hold the bare flag.
    pub cancel: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    /// Speed multiplier in thousandths (1000 = ×1.0). An `AtomicU32` because
    /// there is no atomic `f64`; the fixed point costs nothing at this scale.
    speed_milli: Arc<AtomicU32>,
}

/// Speed clamp. Beyond ×4 the cadence model stops looking human at all, and
/// below ×0.25 a long prompt outlasts the demo.
const SPEED_MIN: f64 = 0.25;
const SPEED_MAX: f64 = 4.0;
/// One press of the faster/slower hotkey.
pub const SPEED_STEP: f64 = 1.25;
/// How long the typer sleeps between pause-flag polls while parked.
const PAUSE_POLL: Duration = Duration::from_millis(20);

impl PlaybackControl {
    pub fn new() -> Self {
        Self::with_cancel(Arc::new(AtomicBool::new(false)))
    }

    pub fn with_cancel(cancel: Arc<AtomicBool>) -> Self {
        Self {
            cancel,
            paused: Arc::new(AtomicBool::new(false)),
            speed_milli: Arc::new(AtomicU32::new(1000)),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Flip pause state; returns the new value.
    pub fn toggle_paused(&self) -> bool {
        let new = !self.is_paused();
        self.set_paused(new);
        new
    }

    pub fn speed(&self) -> f64 {
        self.speed_milli.load(Ordering::Relaxed) as f64 / 1000.0
    }

    /// Multiply the current speed by `factor`, clamped. Returns the new speed.
    pub fn nudge_speed(&self, factor: f64) -> f64 {
        let next = (self.speed() * factor).clamp(SPEED_MIN, SPEED_MAX);
        self.speed_milli
            .store((next * 1000.0).round() as u32, Ordering::Relaxed);
        next
    }

    pub fn reset(&self) {
        self.set_paused(false);
        self.speed_milli.store(1000, Ordering::Relaxed);
    }
}

impl Default for PlaybackControl {
    fn default() -> Self {
        Self::new()
    }
}

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
    play_controlled(
        schedule,
        injector,
        &PlaybackControl::with_cancel(cancel),
        focus_lost,
    )
}

/// Like `play_guarded`, but honors pause and live speed changes from
/// `control`.
///
/// ## Timing model
/// The schedule holds *absolute* times so per-key drift can't accumulate
/// (§3.4). Pause and speed break the "one fixed origin" assumption, so the
/// loop keeps a rebasable origin instead: real time for the next key is
/// `origin_real + (key_time - origin_virtual) / speed`. The origin is rebased
/// only when the user pauses or changes speed, so within any constant-speed
/// stretch the schedule is still absolute and drift-free.
pub fn play_controlled(
    schedule: &[ScheduledKey],
    injector: &mut dyn Injector,
    control: &PlaybackControl,
    focus_lost: Option<&dyn Fn() -> bool>,
) -> PlayOutcome {
    let mut visible_chars = 0usize;
    let focus_throttle = Duration::from_millis(100);
    let mut last_focus_check = Instant::now();
    // Rebasable mapping between schedule time and wall-clock time.
    let mut origin_real = Instant::now();
    let mut origin_virtual_ms: u64 = 0;
    let mut speed = control.speed();

    // Returns Some(outcome) if we should stop now (cancel or focus change).
    let should_stop = |visible_chars: usize, last: &mut Instant| -> Option<PlayOutcome> {
        if control.is_cancelled() {
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
        // Park while paused. Modifiers are released on the way in so a held
        // Shift can't leak into whatever the user does during the pause.
        if control.is_paused() {
            injector.release_all_modifiers();
            while control.is_paused() {
                if let Some(outcome) = should_stop(visible_chars, &mut last_focus_check) {
                    return outcome;
                }
                thread::sleep(PAUSE_POLL);
            }
            // Resuming: the gap ahead should play from now, not from whenever
            // the schedule said it would have.
            origin_virtual_ms = sk.absolute_time_ms.min(origin_virtual_ms);
            origin_real = Instant::now();
        }
        // A speed change also invalidates the current origin.
        let current_speed = control.speed();
        if (current_speed - speed).abs() > f64::EPSILON {
            origin_virtual_ms =
                origin_virtual_ms.max(elapsed_virtual_ms(origin_virtual_ms, origin_real, speed));
            origin_real = Instant::now();
            speed = current_speed;
        }

        let virtual_delta = sk.absolute_time_ms.saturating_sub(origin_virtual_ms) as f64;
        let real_delta_us = (virtual_delta * 1000.0 / speed.max(f64::EPSILON)) as u64;
        let target = origin_real + Duration::from_micros(real_delta_us);
        loop {
            let now = Instant::now();
            if now >= target {
                break;
            }
            let remaining = target - now;
            // Sleep in <=50ms chunks so cancel / focus / pause / speed stay
            // responsive even across a multi-second paragraph pause.
            if remaining > Duration::from_millis(50) {
                if let Some(outcome) = should_stop(visible_chars, &mut last_focus_check) {
                    injector.release_all_modifiers();
                    return outcome;
                }
                if control.is_paused() || (control.speed() - speed).abs() > f64::EPSILON {
                    // Handled at the top of the next iteration; re-dispatch.
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            } else {
                thread::sleep(remaining);
                break;
            }
        }
        // If a pause or speed change landed during the wait we emit this key
        // now rather than dropping it, and the next iteration rebases. Worst
        // case one key plays early — which is what "faster" asked for anyway.
        match sk.key {
            // A raw newline char can still reach here from a hand-built
            // schedule (the CLI, tests). Treat it as "line break, don't
            // submit" — `schedule()` itself now emits explicit key gestures.
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
            Key::ShiftEnter => {
                injector.press_shift_enter();
                visible_chars += 1;
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

/// Schedule-time position reached, given an origin and the speed since it.
fn elapsed_virtual_ms(origin_virtual_ms: u64, origin_real: Instant, speed: f64) -> u64 {
    origin_virtual_ms + (origin_real.elapsed().as_micros() as f64 * speed / 1000.0) as u64
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
            newline_mode: Default::default(),
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
            newline_mode: Default::default(),
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
