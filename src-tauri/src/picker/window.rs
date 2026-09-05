//! §5.4 — the picker is hidden from screen capture by default, via
//! `NSWindowSharingNone` on macOS and `WDA_EXCLUDEFROMCAPTURE` on Windows.
//! Recorders and capture cards render it black. No toggle.

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
    //! `WDA_EXCLUDEFROMCAPTURE` on the picker's top-level HWND.
    //! `SetWindowDisplayAffinity` rejects child windows, WebView2's included,
    //! so there is no descendant walk — see `windows::capture`.
    use crate::platform::windows::capture::apply_display_affinity;
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
        apply_display_affinity(hwnd, affinity).map(|_| ())
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

/// Prepare the picker for first show: capture-exclude, then center on the
/// cursor's screen rather than the OS "main" one.
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
