//! §5.4 — picker window: hide from screen capture by default.
//!
//! - macOS: `[NSWindow setSharingType: NSWindowSharingNone]`.
//! - Windows: `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)`.
//!
//! Both APIs cause OS-level screen recorders, broadcasters, and capture-card
//! drivers to render this window as black/missing. Default-on with no toggle
//! — rehearsal recording of the picker UI itself is a niche we don't support.

use tauri::WebviewWindow;

#[cfg(target_os = "macos")]
mod plat {
    #![allow(deprecated)]

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
    //! Windows applies `WDA_EXCLUDEFROMCAPTURE` to the picker's parent HWND
    //! **and every descendant**. WebView2 hosts its GPU swap chain in a
    //! descendant HWND; setting the flag only on the parent leaves the swap
    //! chain visible-to-capture on some Win11 24H2 configurations, which is
    //! the failure mode that produced "picker is invisible during Zoom
    //! share." See `platform/windows/capture.rs::apply_affinity_recursive`.
    use crate::platform::windows::capture::apply_affinity_recursive;
    use tauri::WebviewWindow;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{WDA_EXCLUDEFROMCAPTURE, WDA_NONE};

    pub fn set_screen_capture_exclusion(window: &WebviewWindow, hide: bool) -> Result<(), String> {
        let hwnd = HWND(window.hwnd().map_err(|e| format!("hwnd: {}", e))?.0 as _);
        let affinity = if hide {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        apply_affinity_recursive(hwnd, affinity).map(|_| ())
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

/// Configure the picker window for first show: capture-exclude, position
/// centered on the screen the user is currently looking at (the one that
/// contains the cursor) — not the OS "main" screen, which would put the
/// picker on the wrong display when the user is on a secondary monitor or
/// inside a fullscreen app on another space.
pub fn prepare_picker(app: &tauri::AppHandle, hide_from_capture: bool) -> Result<(), String> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("picker") {
        #[cfg(target_os = "macos")]
        crate::platform::macos::position_centered_on_cursor(&w, 0.30);
        #[cfg(target_os = "windows")]
        crate::platform::windows::position_centered_on_cursor(&w, 0.30);
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = w.center();
        apply_screen_capture_exclusion(&w, hide_from_capture)?;
    }
    Ok(())
}
