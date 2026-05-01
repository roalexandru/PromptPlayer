//! Windows activation helpers — mirror the macOS module's surface so call
//! sites can use a single `crate::platform::*` import per cfg.
//!
//! - `activate_app` is a no-op on Windows. macOS needs an explicit
//!   `[NSApp activateIgnoringOtherApps:YES]` for an `.accessory` (Dockless)
//!   app to bring its windows forward; Windows has no equivalent gate —
//!   `ShowWindow + SetForegroundWindow` already does the right thing.
//! - `order_panel_front_no_activate` shows the popup WITHOUT activating it,
//!   relying on the `WS_EX_NOACTIVATE` ex-style applied by
//!   `configure_popover_window`.

use tauri::WebviewWindow;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowPos, ShowWindow, HWND_TOPMOST, SET_WINDOW_POS_FLAGS, SHOW_WINDOW_CMD,
};

const SWP_NOMOVE: SET_WINDOW_POS_FLAGS = SET_WINDOW_POS_FLAGS(0x0002);
const SWP_NOSIZE: SET_WINDOW_POS_FLAGS = SET_WINDOW_POS_FLAGS(0x0001);
const SWP_NOACTIVATE: SET_WINDOW_POS_FLAGS = SET_WINDOW_POS_FLAGS(0x0010);
const SWP_SHOWWINDOW: SET_WINDOW_POS_FLAGS = SET_WINDOW_POS_FLAGS(0x0040);
const SW_SHOWNOACTIVATE: SHOW_WINDOW_CMD = SHOW_WINDOW_CMD(4);

/// No-op on Windows — see module docs.
pub fn activate_app() {}

/// Show the popup window without activating it. With `WS_EX_NOACTIVATE` set
/// (see `panel::configure_popover_window`), this shows the window topmost and
/// the user's foreground app keeps its caret.
pub fn order_panel_front_no_activate(window: &WebviewWindow) {
    let Ok(hwnd) = window.hwnd() else { return };
    let hwnd = HWND(hwnd.0 as _);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}
