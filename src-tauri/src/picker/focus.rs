//! §5.5 — capture/restore foreground app for the picker.
//!
//! Strategy:
//!  - On picker open: snapshot foreground app (Mac: `NSWorkspace.frontmostApplication`,
//!    Win: `GetForegroundWindow`).
//!  - On select: hide picker, re-activate previous app, wait ~150ms, deliver prompt.
//!  - Win additionally needs `AttachThreadInput` workaround for focus-stealing prevention.

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
}

/// Recommended delay between focus restoration and first keystroke.
pub const RESTORATION_DELAY: Duration = Duration::from_millis(150);

#[cfg(target_os = "macos")]
fn capture_foreground() -> ForegroundSnapshot {
    use cocoa::base::{id, nil};
    use cocoa::foundation::NSString;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: id = msg_send![workspace, frontmostApplication];
        if app == nil {
            return ForegroundSnapshot::default();
        }
        let bundle: id = msg_send![app, bundleIdentifier];
        let pid: i32 = msg_send![app, processIdentifier];
        let bundle_id = if bundle != nil {
            let utf8: *const std::os::raw::c_char = msg_send![bundle, UTF8String];
            if !utf8.is_null() {
                Some(std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
            } else {
                None
            }
        } else {
            None
        };
        ForegroundSnapshot {
            bundle_id,
            executable: None,
            window_title: None,
            captured_at: Some(Instant::now()),
            handle: Some(pid as u64),
        }
    }
}

#[cfg(target_os = "macos")]
fn restore_to(pid: u64) -> bool {
    use cocoa::base::{id, nil, BOOL, YES};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: id =
            msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid as i32];
        if app == nil {
            return false;
        }
        // Activation options: NSApplicationActivateIgnoringOtherApps = 1<<1
        let opts: u64 = 1 << 1;
        let res: BOOL = msg_send![app, activateWithOptions: opts];
        let _ = workspace;
        res == YES
    }
}

#[cfg(target_os = "windows")]
fn capture_foreground() -> ForegroundSnapshot {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == 0 {
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
    use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    unsafe { SetForegroundWindow(HWND(hwnd as _)).as_bool() }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn capture_foreground() -> ForegroundSnapshot {
    ForegroundSnapshot::default()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn restore_to(_handle: u64) -> bool {
    false
}
