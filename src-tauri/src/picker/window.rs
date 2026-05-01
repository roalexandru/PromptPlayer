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

/// Configure the picker window for first show: capture-exclude, position
/// centered on the screen the user is currently looking at (the one that
/// contains the cursor) — not the OS "main" screen, which would put the
/// picker on the wrong display when the user is on a secondary monitor or
/// inside a fullscreen app on another space.
pub fn prepare_picker(app: &tauri::AppHandle, hide_from_capture: bool) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("picker") {
        center_on_active_screen(&w);
        apply_screen_capture_exclusion(&w, hide_from_capture)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn center_on_active_screen(window: &tauri::WebviewWindow) {
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSPoint, NSRect};
    use objc::{class, msg_send, sel, sel_impl};

    let Ok(ns_window_ptr) = window.ns_window() else { return; };

    unsafe {
        let ns_window: id = ns_window_ptr as id;
        // Window's current frame in AppKit coords (points, bottom-left origin).
        let win_frame: NSRect = msg_send![ns_window, frame];

        // NSEvent.mouseLocation returns the cursor in screen coordinates
        // (origin bottom-left of the *primary* screen, points). Walk
        // NSScreen.screens to find which screen contains the cursor.
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let screens: id = msg_send![class!(NSScreen), screens];
        if screens == nil {
            let _ = window.center();
            return;
        }
        let count: usize = msg_send![screens, count];
        let mut chosen_frame: Option<NSRect> = None;
        for i in 0..count {
            let screen: id = msg_send![screens, objectAtIndex: i];
            let frame: NSRect = msg_send![screen, frame];
            if cursor.x >= frame.origin.x
                && cursor.x <= frame.origin.x + frame.size.width
                && cursor.y >= frame.origin.y
                && cursor.y <= frame.origin.y + frame.size.height
            {
                chosen_frame = Some(frame);
                break;
            }
        }
        let Some(frame) = chosen_frame else {
            let _ = window.center();
            return;
        };

        // Position centered horizontally on the chosen screen, 30% from the
        // top (Spotlight-style). All math in AppKit bottom-up coords; we
        // call setFrameOrigin: directly so no conversion to Tauri's
        // top-left coord system is needed.
        let x = frame.origin.x + (frame.size.width - win_frame.size.width) / 2.0;
        let y_top_pad = frame.size.height * 0.30;
        let y = frame.origin.y + frame.size.height - y_top_pad - win_frame.size.height;
        let origin = NSPoint { x, y };
        let _: () = msg_send![ns_window, setFrameOrigin: origin];
    }
}

#[cfg(not(target_os = "macos"))]
fn center_on_active_screen(window: &tauri::WebviewWindow) {
    let _ = window.center();
}
