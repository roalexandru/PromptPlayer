//! §5.5 — snapshot the foreground app on picker open; on select, hide,
//! re-activate, confirm the transfer happened, then deliver.
//!
//! Windows needs `AttachThreadInput` to get past focus-stealing prevention.

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

    /// Restore focus to the captured app. True if activation succeeded — the
    /// caller must still wait `RESTORATION_DELAY` before typing.
    pub fn restore(&self) -> bool {
        let snap = self.snapshot();
        if let Some(handle) = snap.handle {
            restore_to(handle)
        } else {
            false
        }
    }

    /// Restore focus and block until the OS confirms the transfer, or `timeout`.
    /// Required before paste: Ctrl/Cmd+V goes to whatever is foreground *now*.
    pub fn restore_and_wait(&self, timeout: Duration) -> bool {
        let snap = self.snapshot();
        let Some(handle) = snap.handle else {
            return false;
        };
        // Activate regardless of the return: SetForegroundWindow reports FALSE
        // under focus-stealing prevention yet often lands anyway. The poll gates.
        let _ = restore_to(handle);
        wait_until_foreground(handle, timeout)
    }
}

/// Blind fallback delay, used only where verification isn't available or the
/// confirmation poll times out.
pub const RESTORATION_DELAY: Duration = Duration::from_millis(150);

/// Upper bound for `restore_and_wait` — generous because a loaded target can
/// take >100ms, though we return as soon as the transfer is observed.
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
    use crate::platform::windows::capture::{
        class_name_of, collect_z_order_candidates, foreground_hwnd, select_target, window_title_of,
    };
    let fg = foreground_hwnd();
    if fg.0.is_null() {
        return ForegroundSnapshot::default();
    }
    let fg_class = class_name_of(fg);

    // Collect z-order candidates, then let the pure `select_target` policy
    // pick the first plausible focus-restore target.
    let candidates = collect_z_order_candidates(fg, 10);
    let (handle_raw, window_title) = match select_target(&candidates) {
        Some(c) => (c.hwnd_raw, c.title.clone()),
        None => {
            // Nothing passed the filter — fall back to the raw foreground
            // HWND rather than silently dropping the snapshot.
            tracing::warn!(
                target: "prompt_player::capture",
                fg_hwnd = fg.0 as usize,
                fg_class = %fg_class,
                "no acceptable target in z-order; using raw foreground HWND"
            );
            (fg.0 as u64, window_title_of(fg))
        }
    };

    tracing::info!(
        target: "prompt_player::capture",
        fg_hwnd = fg.0 as usize,
        fg_class = %fg_class,
        picked_hwnd = handle_raw as usize,
        title = ?window_title,
        "capture_foreground snapshot"
    );

    ForegroundSnapshot {
        bundle_id: None,
        executable: None,
        window_title,
        captured_at: Some(Instant::now()),
        handle: Some(handle_raw),
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
        // We just hid the picker, so we're usually still foreground and
        // focus-stealing prevention doesn't apply.
        if SetForegroundWindow(target).as_bool() {
            return true;
        }
        // Attaching to the foreground thread's input queue inherits its
        // "input event" status, which gets SetForegroundWindow through.
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
            tracing::debug!(
                target: "prompt_player::capture",
                target_hwnd = hwnd as usize,
                elapsed_ms = start.elapsed().as_millis() as u64,
                "wait_until_foreground: target became foreground"
            );
            return true;
        }
        if start.elapsed() >= timeout {
            tracing::warn!(
                target: "prompt_player::capture",
                target_hwnd = hwnd as usize,
                fg_hwnd = fg.0 as usize,
                timeout_ms = timeout.as_millis() as u64,
                "wait_until_foreground: timed out before target became foreground"
            );
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
