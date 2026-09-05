//! "Keep Awake" — inhibit display sleep, the screensaver and idle system sleep.
//! Starts OFF each launch like `armed` (§10.1) unless `restore_keep_awake` is on.
//!
//! Every enable carries a deadline: "on" used to end only when the user
//! remembered, and the field data has a ~10h median with one run at 3d14h.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the caller should drive [`PowerManager::expire_if_due`]. Coarse
/// on purpose — it only needs to keep "2 hours" from visibly overshooting.
pub const EXPIRY_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Cross-platform keep-awake controller. Cheap to clone via `Arc`; lives on
/// [`crate::app::context::AppContext`].
pub struct PowerManager {
    enabled: AtomicBool,
    /// When the session auto-releases; `None` if disabled or indefinite.
    deadline: parking_lot::Mutex<Option<Instant>>,
    /// Minutes the session started with (`0` = indefinite), so the UI can
    /// render "2h" without recomputing from the deadline.
    duration_mins: std::sync::atomic::AtomicU16,
    /// Held IOKit assertion id (`IOPMAssertionID`), if any.
    #[cfg(target_os = "macos")]
    assertion: parking_lot::Mutex<Option<u32>>,
    /// Channel to the owner thread that owns the thread-affine
    /// `SetThreadExecutionState` state. `true` = keep awake, `false` = release.
    #[cfg(target_os = "windows")]
    tx: crossbeam_channel::Sender<bool>,
}

impl PowerManager {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        let tx = {
            let (tx, rx) = crossbeam_channel::unbounded::<bool>();
            windows_impl::spawn_owner_thread(rx);
            tx
        };
        Self {
            enabled: AtomicBool::new(false),
            deadline: parking_lot::Mutex::new(None),
            duration_mins: std::sync::atomic::AtomicU16::new(0),
            #[cfg(target_os = "macos")]
            assertion: parking_lot::Mutex::new(None),
            #[cfg(target_os = "windows")]
            tx,
        }
    }

    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Minutes the session started with. `0` = indefinite or no session —
    /// check [`Self::is_enabled`] to tell those apart.
    pub fn duration_mins(&self) -> u16 {
        self.duration_mins.load(Ordering::Relaxed)
    }

    /// Time left before auto-off. `None` when disabled or indefinite.
    pub fn remaining(&self) -> Option<Duration> {
        self.remaining_at(Instant::now())
    }

    fn remaining_at(&self, now: Instant) -> Option<Duration> {
        self.deadline
            .lock()
            .map(|d| d.saturating_duration_since(now))
    }

    /// Enable/disable with an auto-off in minutes; `0` is indefinite, but only
    /// when explicitly asked for. Returns the new state.
    pub fn set_for(&self, enabled: bool, duration_mins: u16) -> bool {
        self.set_for_at(enabled, duration_mins, Instant::now())
    }

    fn set_for_at(&self, enabled: bool, duration_mins: u16, now: Instant) -> bool {
        self.apply(enabled);
        self.enabled.store(enabled, Ordering::Relaxed);
        let mins = if enabled { duration_mins } else { 0 };
        self.duration_mins.store(mins, Ordering::Relaxed);
        *self.deadline.lock() = if enabled && duration_mins > 0 {
            Some(now + Duration::from_secs(duration_mins as u64 * 60))
        } else {
            None
        };
        enabled
    }

    /// Flip the current state, starting a `duration_mins` session when turning
    /// on. Returns the new value.
    pub fn toggle_for(&self, duration_mins: u16) -> bool {
        self.set_for(!self.is_enabled(), duration_mins)
    }

    /// Release the assertion if the deadline passed. True only on the
    /// transition, so the caller emits one `KeepAwakeExpired`.
    pub fn expire_if_due(&self) -> bool {
        self.expire_if_due_at(Instant::now())
    }

    fn expire_if_due_at(&self, now: Instant) -> bool {
        if !self.is_enabled() {
            return false;
        }
        let due = matches!(*self.deadline.lock(), Some(d) if now >= d);
        if !due {
            return false;
        }
        tracing::info!("keep-awake auto-off — session deadline reached");
        self.set_for_at(false, 0, now);
        true
    }

    /// Set state with no deadline. Indefinite by construction; prefer
    /// [`Self::set_for`] on user-facing paths.
    pub fn set(&self, enabled: bool) -> bool {
        self.set_for(enabled, 0)
    }

    /// Flip state, indefinite. Prefer [`Self::toggle_for`] for users.
    pub fn toggle(&self) -> bool {
        let next = !self.is_enabled();
        self.set(next)
    }

    #[cfg(target_os = "macos")]
    fn apply(&self, enabled: bool) {
        let mut guard = self.assertion.lock();
        if enabled {
            // Already asserted — nothing to do (idempotent).
            if guard.is_some() {
                return;
            }
            if let Some(id) = macos_ffi::create() {
                tracing::info!("keep-awake ON — display-sleep assertion {}", id);
                *guard = Some(id);
            }
        } else if let Some(id) = guard.take() {
            macos_ffi::release(id);
            tracing::info!("keep-awake OFF — released assertion {}", id);
        }
    }

    #[cfg(target_os = "windows")]
    fn apply(&self, enabled: bool) {
        if self.tx.send(enabled).is_err() {
            tracing::warn!(
                "keep-awake: owner thread gone; cannot {} execution state",
                if enabled { "set" } else { "clear" }
            );
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    fn apply(&self, _enabled: bool) {
        // No supported power API on this target — flag-only.
    }
}

impl Default for PowerManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "macos")]
mod macos_ffi {
    //! Raw IOKit power-assertion FFI, same shape as the Accessibility FFI in
    //! `crate::tcc`.
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            assertion_level: u32,
            assertion_name: CFStringRef,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }

    /// `kIOPMAssertionLevelOn`.
    const ASSERTION_LEVEL_ON: u32 = 255;

    /// Create a `PreventUserIdleDisplaySleep` assertion. Returns the assertion
    /// id on success (`kIOReturnSuccess == 0`).
    pub fn create() -> Option<u32> {
        let assertion_type = CFString::from_static_string("PreventUserIdleDisplaySleep");
        let name = CFString::new("Prompt Player Keep Awake");
        let mut id: u32 = 0;
        // SAFETY: both CFString args outlive the call; IOKit copies/retains the
        // type and name internally. `id` is a valid out-pointer.
        let ret = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type.as_concrete_TypeRef(),
                ASSERTION_LEVEL_ON,
                name.as_concrete_TypeRef(),
                &mut id,
            )
        };
        if ret == 0 {
            Some(id)
        } else {
            tracing::warn!("IOPMAssertionCreateWithName failed: {:#x}", ret);
            None
        }
    }

    /// Release a previously created assertion.
    pub fn release(id: u32) {
        // SAFETY: `id` was returned by a successful `create()` and hasn't been
        // released yet (guarded by the `Option` in `PowerManager`).
        let ret = unsafe { IOPMAssertionRelease(id) };
        if ret != 0 {
            tracing::warn!("IOPMAssertionRelease({}) failed: {:#x}", id, ret);
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use crossbeam_channel::Receiver;
    use std::thread;
    use windows::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        EXECUTION_STATE,
    };

    /// Owns the thread-affine execution state. Everything reapplies on this one
    /// thread, so an exiting pool thread can't clear `ES_CONTINUOUS`.
    pub fn spawn_owner_thread(rx: Receiver<bool>) {
        thread::Builder::new()
            .name("prompt-player-keep-awake".into())
            .spawn(move || {
                while let Ok(enabled) = rx.recv() {
                    let flags: EXECUTION_STATE = if enabled {
                        ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
                    } else {
                        // ES_CONTINUOUS alone clears the SYSTEM/DISPLAY
                        // requirements, letting normal idle timers resume.
                        ES_CONTINUOUS
                    };
                    // SAFETY: plain Win32 call with no pointer args.
                    let prev = unsafe { SetThreadExecutionState(flags) };
                    if prev == EXECUTION_STATE(0) {
                        tracing::warn!("SetThreadExecutionState failed");
                    } else {
                        tracing::info!(
                            "keep-awake {} (execution state updated)",
                            if enabled { "ON" } else { "OFF" }
                        );
                    }
                }
            })
            .expect("spawn keep-awake owner thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_disabled() {
        let p = PowerManager::new();
        assert!(!p.is_enabled(), "keep-awake starts off every launch");
    }

    #[test]
    fn toggle_flips_and_returns_new_state() {
        // Exercises the real OS assertion on CI — no permission needed, safe
        // headless.
        let p = PowerManager::new();
        assert!(p.toggle(), "first toggle enables");
        assert!(p.is_enabled());
        assert!(!p.toggle(), "second toggle disables");
        assert!(!p.is_enabled());
    }

    #[test]
    fn set_is_idempotent() {
        let p = PowerManager::new();
        assert!(p.set(true));
        assert!(p.set(true), "re-enabling stays enabled");
        assert!(p.is_enabled());
        assert!(!p.set(false));
        assert!(!p.set(false), "re-disabling stays disabled");
        assert!(!p.is_enabled());
    }

    #[test]
    fn timed_session_expires_exactly_once() {
        let p = PowerManager::new();
        let t0 = Instant::now();
        p.set_for_at(true, 60, t0);
        assert!(p.is_enabled());
        assert_eq!(p.duration_mins(), 60);

        // Not yet due.
        assert!(!p.expire_if_due_at(t0 + Duration::from_secs(59 * 60)));
        assert!(p.is_enabled(), "must stay on until the deadline");

        // Due — releases and reports the transition.
        assert!(p.expire_if_due_at(t0 + Duration::from_secs(60 * 60)));
        assert!(!p.is_enabled());
        // ...and only once, so the caller emits one KeepAwakeExpired.
        assert!(!p.expire_if_due_at(t0 + Duration::from_secs(90 * 60)));
    }

    #[test]
    fn indefinite_session_never_expires() {
        // `0` remains available, but only when explicitly chosen.
        let p = PowerManager::new();
        let t0 = Instant::now();
        p.set_for_at(true, 0, t0);
        assert!(p.remaining_at(t0).is_none());
        // The 3d14h session from the field data — still on, by request.
        assert!(!p.expire_if_due_at(t0 + Duration::from_secs(86_400 * 4)));
        assert!(p.is_enabled());
    }

    #[test]
    fn remaining_counts_down_and_saturates() {
        let p = PowerManager::new();
        let t0 = Instant::now();
        p.set_for_at(true, 30, t0);
        assert_eq!(
            p.remaining_at(t0 + Duration::from_secs(600)),
            Some(Duration::from_secs(1200))
        );
        // Past the deadline the remaining time floors at zero rather than
        // underflowing the Duration subtraction.
        assert_eq!(
            p.remaining_at(t0 + Duration::from_secs(9999)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn turning_off_clears_the_deadline() {
        let p = PowerManager::new();
        let t0 = Instant::now();
        p.set_for_at(true, 30, t0);
        p.set_for_at(false, 0, t0 + Duration::from_secs(10));
        assert!(p.remaining().is_none());
        assert_eq!(p.duration_mins(), 0);
        assert!(!p.expire_if_due_at(t0 + Duration::from_secs(9999)));
    }

    #[test]
    fn re_enabling_restarts_the_clock() {
        // Re-picking mid-session extends from now, not the original deadline.
        let p = PowerManager::new();
        let t0 = Instant::now();
        p.set_for_at(true, 30, t0);
        p.set_for_at(true, 30, t0 + Duration::from_secs(29 * 60));
        assert!(!p.expire_if_due_at(t0 + Duration::from_secs(30 * 60)));
        assert!(p.expire_if_due_at(t0 + Duration::from_secs(59 * 60)));
    }

    #[test]
    fn toggle_for_starts_a_bounded_session() {
        let p = PowerManager::new();
        assert!(p.toggle_for(120));
        assert_eq!(p.duration_mins(), 120);
        assert!(p.remaining().is_some());
        assert!(!p.toggle_for(120));
        assert!(p.remaining().is_none());
    }

    #[test]
    fn expire_is_a_noop_while_disabled() {
        let p = PowerManager::new();
        assert!(!p.expire_if_due());
    }
}
