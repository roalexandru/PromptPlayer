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

/// Prepare the picker for first show: capture-exclude, then position it.
///
/// Called from app setup so the exclusion is in place before the window is
/// ever composited, and re-asserted on every show by the picker command
/// module — a window that was hidden and reshown can lose the affinity flag,
/// and on macOS the sharing type is per-window state worth re-asserting.
///
/// The function that shows the window is deliberately not named here: a
/// contract test greps for external references to keep it private, so that
/// every picker entry point goes through a reporting wrapper.
pub fn prepare_picker(app: &tauri::AppHandle, hide_from_capture: bool) -> Result<(), String> {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("picker") {
        position_picker(app, crate::config::PickerDisplay::default());
        apply_screen_capture_exclusion(&w, hide_from_capture)?;
    }
    Ok(())
}

/// Place the picker according to the configured display preference.
///
/// `Auto` is the interesting case. Capture exclusion keeps the picker out of a
/// Zoom share, but an extended-desktop projector is a second physical output —
/// the OS composites the picker onto it like any other window. So when the
/// desktop is extended we place the picker on the primary display (the
/// presenter's own screen) rather than following the cursor onto the
/// projector. Mirrored setups are one logical screen, so cursor-following is
/// correct there and `Auto` leaves it alone.
pub fn position_picker(app: &tauri::AppHandle, display: crate::config::PickerDisplay) {
    use crate::config::PickerDisplay;
    use tauri::Manager;
    let Some(w) = app.get_webview_window("picker") else {
        return;
    };
    const TOP_PADDING: f64 = 0.30;

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        #[cfg(target_os = "macos")]
        use crate::platform::macos as plat;
        #[cfg(target_os = "windows")]
        use crate::platform::windows as plat;

        let on_primary = match display {
            PickerDisplay::Cursor => false,
            PickerDisplay::Builtin => true,
            PickerDisplay::Auto => plat::is_extended_desktop(),
        };
        if on_primary {
            plat::position_centered_on_primary(&w, TOP_PADDING);
        } else {
            plat::position_centered_on_cursor(&w, TOP_PADDING);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = display;
        let _ = w.center();
    }
}
