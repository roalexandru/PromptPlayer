//! §5.4 — picker window: hide from screen capture by default.
//!
//! - macOS: `[NSWindow setSharingType: NSWindowSharingNone]`.
//! - Windows: `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)`.
//!
//! Both APIs cause OS-level screen recorders, broadcasters, and capture-card
//! drivers to render this window as black/missing. Toggle in settings ("Show
//! during screen sharing") for rehearsal mode (Phase 13).

use tauri::{Manager, WebviewWindow};

#[cfg(target_os = "macos")]
mod plat {
    use cocoa::base::id;
    use objc::{msg_send, sel, sel_impl};
    use tauri::WebviewWindow;

    /// `NSWindowSharingNone = 0`.
    const NS_WINDOW_SHARING_NONE: u64 = 0;
    /// `NSWindowSharingReadOnly = 1`.
    const NS_WINDOW_SHARING_READ_ONLY: u64 = 1;

    pub fn set_screen_capture_exclusion(window: &WebviewWindow, hide: bool) -> Result<(), String> {
        let ns_win: id = window
            .ns_window()
            .map_err(|e| format!("ns_window: {}", e))? as id;
        let kind = if hide {
            NS_WINDOW_SHARING_NONE
        } else {
            NS_WINDOW_SHARING_READ_ONLY
        };
        unsafe {
            let _: () = msg_send![ns_win, setSharingType: kind];
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod plat {
    use tauri::WebviewWindow;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    };

    pub fn set_screen_capture_exclusion(window: &WebviewWindow, hide: bool) -> Result<(), String> {
        let hwnd = HWND(window.hwnd().map_err(|e| format!("hwnd: {}", e))?.0 as _);
        let affinity = if hide { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        unsafe {
            SetWindowDisplayAffinity(hwnd, affinity).map_err(|e| format!("SetWindowDisplayAffinity: {}", e))?;
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod plat {
    use tauri::WebviewWindow;
    pub fn set_screen_capture_exclusion(_w: &WebviewWindow, _hide: bool) -> Result<(), String> {
        Ok(())
    }
}

/// Apply the §5.4 screen-capture exclusion to the picker window.
/// `hide=true` (default) makes the window invisible to recorders.
pub fn apply_screen_capture_exclusion(window: &WebviewWindow, hide: bool) -> Result<(), String> {
    plat::set_screen_capture_exclusion(window, hide)
}

/// Configure the picker window for first show: capture-exclude, center on screen.
pub fn prepare_picker(app: &tauri::AppHandle, hide_from_capture: bool) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("picker") {
        let _ = w.center();
        apply_screen_capture_exclusion(&w, hide_from_capture)?;
    }
    Ok(())
}
