//! "Keep Awake" — inhibit display sleep, the screensaver, and idle system
//! sleep on demand, so a live demo / long-running session isn't interrupted.
//!
//! State lives in-memory only and starts OFF every launch — same philosophy as
//! the `armed` toggle in [`crate::state::AppState`] (see §10.1). There is no
//! persisted settings store to hang this on, and re-enabling before a demo is
//! cheap.
//!
//! Platform split (mirrors the `#[cfg]` + `#[cfg(not)]`-stub idiom used in
//! `crate::tcc`):
//! - **macOS**: an IOKit power assertion of type `PreventUserIdleDisplaySleep`.
//!   Keeping the display awake also blocks the screensaver and idle *system*
//!   sleep, so one assertion covers everything the user asked for.
//! - **Windows**: `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED |
//!   ES_DISPLAY_REQUIRED)`. That call is thread-affine — the continuous state
//!   is cleared when the setting thread exits — so every call is funnelled
//!   through one long-lived owner thread (same named-thread pattern as
//!   `platform::windows::tray_theme`).
//! - **Other targets**: no-op that only tracks the flag (keeps the workspace
//!   building on Linux dev machines).
//!
//! Cleanup is automatic on both real platforms: IOKit releases dangling
//! assertions and Windows clears `ES_CONTINUOUS` when the process exits, so
//! there's no explicit quit-time teardown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cross-platform keep-awake controller. Cheap to clone via `Arc`; lives on
/// [`crate::app::context::AppContext`].
pub struct PowerManager {
    enabled: AtomicBool,
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

    /// Set the keep-awake state, applying the OS-level assertion. Returns the
    /// new state (always equal to `enabled`) so callers can echo it back to the
    /// UI without a follow-up read.
    pub fn set(&self, enabled: bool) -> bool {
        self.apply(enabled);
        self.enabled.store(enabled, Ordering::Relaxed);
        enabled
    }

    /// Flip the current state and return the new value.
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
    //! Raw IOKit power-assertion FFI. Same shape as the Accessibility FFI in
    //! `crate::tcc` — declare the C symbols, link the framework, marshal
    //! `CFString` args via `core-foundation` (already a mac dependency).
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

    /// Long-lived thread that owns the thread-affine execution state. Receives
    /// desired-state booleans and (re)applies `SetThreadExecutionState` on this
    /// same thread so `ES_CONTINUOUS` is never cleared out from under us by a
    /// pool thread exiting.
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
        // On macOS/Windows CI runners this exercises the real OS assertion
        // (create+release / set+clear execution state), which needs no special
        // permission and is safe to run headless.
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
}
