//! Windows window-chrome configuration mirroring the macOS NSPanel idiom.
//!
//! On macOS the tray popup is an NSPanel with `NonactivatingPanel` style so it
//! can be key-window without bringing the app foreground (the user's demo
//! target keeps focus). The Windows equivalent is `WS_EX_NOACTIVATE` on the
//! HWND extended style: the window never becomes the foreground window, even
//! when shown or clicked, so the foreground app retains its caret/focus.
//!
//! Also applied: `WS_EX_TOOLWINDOW` (no Alt-Tab entry; complements the
//! `skipTaskbar: true` config on the Tauri window) and `WS_EX_TOPMOST` (float
//! over normal windows). For the picker — which is intentionally activating,
//! Spotlight-style — we set only `WS_EX_TOPMOST`: we want it to take focus so
//! the search input can receive keystrokes.

use tauri::WebviewWindow;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST,
    SET_WINDOW_POS_FLAGS, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, WINDOW_EX_STYLE,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

const SWP_NOACTIVATE: SET_WINDOW_POS_FLAGS = SET_WINDOW_POS_FLAGS(0x0010);

/// Apply `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST` to the tray
/// popup so it never steals focus and floats over normal windows.
pub fn configure_popover_window(window: &WebviewWindow) {
    let extra = (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0) as isize;
    apply_ex_style(window, extra);
}

/// Apply only `WS_EX_TOPMOST` to the picker. The picker is intentionally
/// activating (Spotlight-style — its search input must receive typing), so we
/// do NOT set `WS_EX_NOACTIVATE`.
pub fn configure_picker_window(window: &WebviewWindow) {
    apply_ex_style(window, WS_EX_TOPMOST.0 as isize);
}

/// macOS-only operation; no Windows analogue (Windows has no "Spaces"
/// concept). No-op so the cross-platform call site can be unified.
pub fn make_window_space_neutral(_window: &WebviewWindow) {}

fn apply_ex_style(window: &WebviewWindow, extra: isize) {
    let Ok(hwnd) = window.hwnd() else { return };
    let hwnd = HWND(hwnd.0 as _);
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new = current | extra;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
        // Commit the new ex-style: SetWindowPos with SWP_FRAMECHANGED forces a
        // re-evaluation. SWP_NOACTIVATE prevents this call from itself
        // foregrounding the window.
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// Read the current `GWL_EXSTYLE` of a window. Public for tests / inspection.
#[cfg(test)]
pub fn current_ex_style(window: &WebviewWindow) -> Option<WINDOW_EX_STYLE> {
    let hwnd = window.hwnd().ok()?;
    let hwnd = HWND(hwnd.0 as _);
    let v = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    Some(WINDOW_EX_STYLE(v as u32))
}
