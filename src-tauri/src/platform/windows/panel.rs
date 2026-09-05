//! Windows chrome mirroring the macOS NSPanel idiom. `WS_EX_NOACTIVATE` is the
//! equivalent of `NonactivatingPanel`: the popup never takes foreground, so the
//! demo target keeps its caret. Plus `WS_EX_TOOLWINDOW` (no Alt-Tab) and
//! `WS_EX_TOPMOST`. The picker gets only TOPMOST — it must take focus.

use tauri::WebviewWindow;
use windows::Win32::Foundation::HWND;
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST,
    SET_WINDOW_POS_FLAGS, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

const SWP_NOACTIVATE: SET_WINDOW_POS_FLAGS = SET_WINDOW_POS_FLAGS(0x0010);

/// Apply `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST` to the tray
/// popup so it never steals focus and floats over normal windows.
pub fn configure_popover_window(window: &WebviewWindow) {
    let extra = (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0) as isize;
    apply_ex_style(window, extra);
}

/// TOPMOST only. The picker is deliberately activating — its search input has
/// to receive typing — so no `WS_EX_NOACTIVATE`.
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
        // SWP_FRAMECHANGED forces the new ex-style to be re-evaluated;
        // SWP_NOACTIVATE stops this call itself foregrounding the window.
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
