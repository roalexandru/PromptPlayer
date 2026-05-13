//! §5.5 — capture/restore foreground app for the picker.
//!
//! Strategy:
//!  - On picker open: snapshot foreground app (Mac: `NSWorkspace.frontmostApplication`,
//!    Win: `GetForegroundWindow`).
//!  - On select: hide picker, re-activate previous app, confirm foreground
//!    really transferred, then deliver the prompt.
//!  - Win uses the `AttachThreadInput` workaround for focus-stealing prevention,
//!    then polls `GetForegroundWindow` until it matches the captured hwnd (or
//!    a hard cap fires). That replaces the previous blind ~150ms sleep: we
//!    return as soon as focus is actually restored, and fall back to the cap
//!    only if the OS refuses (e.g., the target window was closed mid-flight).

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct ForegroundSnapshot {
    pub bundle_id: Option<String>,
    pub executable: Option<String>,
    pub window_title: Option<String>,
    pub captured_at: Option<Instant>,
    /// Opaque handle for restoration. Mac: `pid_t`. Win: `HWND` as u64.
    pub handle: Option<u64>,
}

/// Shared store of the most recent foreground snapshot.
#[derive(Default)]
pub struct FocusStore {
    snap: Mutex<ForegroundSnapshot>,
}

impl FocusStore {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn capture(&self) -> ForegroundSnapshot {
        let snap = capture_foreground();
        *self.snap.lock() = snap.clone();
        snap
    }

    pub fn snapshot(&self) -> ForegroundSnapshot {
        self.snap.lock().clone()
    }

    /// Restore focus to the previously-captured app.
    /// Returns true if the activation call succeeded; the caller still must
    /// wait `RESTORATION_DELAY` before delivering keystrokes.
    pub fn restore(&self) -> bool {
        let snap = self.snapshot();
        if let Some(handle) = snap.handle {
            restore_to(handle)
        } else {
            false
        }
    }

    /// Restore focus and block (briefly) until the OS reports the captured
    /// window as foreground. Returns true if the verification succeeded
    /// within `timeout`, false on timeout / no snapshot / refused activation.
    ///
    /// Use this in front of paste-style delivery, where the next keystroke
    /// (Ctrl/Cmd+V) goes to whichever window is foreground *right now* —
    /// guessing with a fixed sleep is exactly what produced the prior
    /// "first chars land in the wrong window" symptom.
    pub fn restore_and_wait(&self, timeout: Duration) -> bool {
        let snap = self.snapshot();
        let Some(handle) = snap.handle else {
            return false;
        };
        // Fire the activation regardless of its bool return — even when
        // the kernel reports failure (SetForegroundWindow returns FALSE
        // under focus-stealing prevention), the transition sometimes
        // completes a few ms later. The poll below is the actual gate.
        let _ = restore_to(handle);
        wait_until_foreground(handle, timeout)
    }
}

/// Fallback delay between focus restoration and first keystroke. Used only
/// when the verification path is not available (non-Win/Mac builds, or as a
/// last-ditch nap if confirmation polling fails). The original code path
/// hardcoded this as a blind sleep on every fire.
pub const RESTORATION_DELAY: Duration = Duration::from_millis(150);

/// Upper bound for `restore_and_wait`. Generous because focus transitions
/// against a hot-loaded target (Slack, Teams, Edge) can take >100ms; we
/// still return as soon as the foreground transfer is observed.
pub const RESTORATION_TIMEOUT: Duration = Duration::from_millis(400);

#[cfg(target_os = "macos")]
fn capture_foreground() -> ForegroundSnapshot {
    let snap = crate::platform::macos::nsworkspace::frontmost_app();
    ForegroundSnapshot {
        bundle_id: snap.bundle_id,
        executable: snap.executable_path.clone(),
        window_title: None,
        captured_at: Some(Instant::now()),
        handle: snap.pid.map(|p| p as u64),
    }
}

#[cfg(target_os = "macos")]
fn restore_to(pid: u64) -> bool {
    crate::platform::macos::nsworkspace::activate_pid(pid as i32)
}

#[cfg(target_os = "windows")]
fn capture_foreground() -> ForegroundSnapshot {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return ForegroundSnapshot::default();
        }
        let mut title = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut title);
        let window_title = if len > 0 {
            Some(String::from_utf16_lossy(&title[..len as usize]))
        } else {
            None
        };
        ForegroundSnapshot {
            bundle_id: None,
            executable: None,
            window_title,
            captured_at: Some(Instant::now()),
            handle: Some(hwnd.0 as u64),
        }
    }
}

#[cfg(target_os = "windows")]
fn restore_to(hwnd: u64) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::SetActiveWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic,
        SetForegroundWindow, ShowWindow, SHOW_WINDOW_CMD,
    };
    const SW_RESTORE: SHOW_WINDOW_CMD = SHOW_WINDOW_CMD(9);
    let target = HWND(hwnd as _);
    unsafe {
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
        }
        // Vanilla path first — when our process is still the foreground
        // (we just hid the picker), focus-stealing prevention doesn't
        // block us and this returns true immediately.
        if SetForegroundWindow(target).as_bool() {
            return true;
        }
        // Fallback: attach our input queue to the current foreground
        // thread's queue, which inherits its "input event" status for the
        // duration. With that, SetForegroundWindow no longer hits focus-
        // stealing prevention. This is the workaround referenced in the
        // module header.
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let target_thread = GetWindowThreadProcessId(target, None);
        let cur_thread = GetCurrentThreadId();
        let attached_fg = AttachThreadInput(cur_thread, fg_thread, true).as_bool();
        let attached_target = if target_thread != cur_thread && target_thread != fg_thread {
            AttachThreadInput(cur_thread, target_thread, true).as_bool()
        } else {
            false
        };
        let _ = BringWindowToTop(target);
        let _ = SetActiveWindow(target);
        let ok = SetForegroundWindow(target).as_bool();
        if attached_target {
            let _ = AttachThreadInput(cur_thread, target_thread, false);
        }
        if attached_fg {
            let _ = AttachThreadInput(cur_thread, fg_thread, false);
        }
        ok
    }
}

#[cfg(target_os = "windows")]
fn wait_until_foreground(hwnd: u64, timeout: Duration) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let target = HWND(hwnd as _);
    let start = Instant::now();
    // 1 ms ticks: cheap kernel call, gives the target's UI thread room to
    // pump WM_ACTIVATE between checks.
    loop {
        let fg = unsafe { GetForegroundWindow() };
        if fg.0 == target.0 {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(target_os = "macos")]
fn wait_until_foreground(pid: u64, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        let snap = crate::platform::macos::nsworkspace::frontmost_app();
        if snap.pid.map(|p| p as u64) == Some(pid) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn capture_foreground() -> ForegroundSnapshot {
    ForegroundSnapshot::default()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn restore_to(_handle: u64) -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn wait_until_foreground(_handle: u64, _timeout: Duration) -> bool {
    false
}
