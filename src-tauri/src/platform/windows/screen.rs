//! Cursor-aware monitor positioning — Windows counterpart to
//! `platform/macos/screen.rs`.
//!
//! Strategy:
//! 1. `GetCursorPos` → cursor in physical screen coordinates.
//! 2. `MonitorFromPoint(MONITOR_DEFAULTTONEAREST)` → handle of the monitor
//!    the cursor is on (with auto-fallback to the closest monitor if the
//!    cursor is somehow off-screen).
//! 3. `GetMonitorInfoW` → `rcWork`, the monitor's work area (excludes the
//!    taskbar). Use the work area, not the full bounds, so the picker isn't
//!    shoved partially behind the taskbar.
//! 4. Place the window centered horizontally, `top_padding_fraction` of the
//!    work-area height down from the top.

use tauri::{AppHandle, Manager, PhysicalPosition};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, SM_CMONITORS};

/// Position the window centered on the cursor's monitor work area, with a
/// fraction of the work-area height as top padding (Spotlight uses ~0.30).
pub fn position_centered_on_cursor(window: &tauri::WebviewWindow, top_padding_fraction: f64) {
    let Some(work) = cursor_monitor_work_area() else {
        return;
    };
    let Ok(outer) = window.outer_size() else {
        return;
    };
    let win_w = outer.width as i32;

    let work_w = work.right - work.left;
    let work_h = work.bottom - work.top;
    let x = work.left + (work_w - win_w) / 2;
    let top_pad = (work_h as f64 * top_padding_fraction) as i32;
    let y = work.top + top_pad;

    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// Position the picker on whichever monitor contains the cursor.
pub fn position_picker_on_cursor_screen(app: &AppHandle) {
    let Some(window) = app.get_webview_window("picker") else {
        return;
    };
    position_centered_on_cursor(&window, 0.30);
}

/// True when the desktop spans more than one physical display.
///
/// Duplicated (mirrored) displays report as a single monitor, so a count above
/// one means the presenter's screen and the projector show different content —
/// the case where cursor-following placement can put the picker on the
/// projector. Capture exclusion hides the picker from a screen *share*; it
/// does nothing about a second physical output.
pub fn is_extended_desktop() -> bool {
    unsafe { GetSystemMetrics(SM_CMONITORS) > 1 }
}

/// Same placement as `position_centered_on_cursor`, but pinned to the primary
/// monitor regardless of where the cursor is.
pub fn position_centered_on_primary(window: &tauri::WebviewWindow, top_padding_fraction: f64) {
    let Some(work) = primary_monitor_work_area() else {
        return;
    };
    let Ok(outer) = window.outer_size() else {
        return;
    };
    let win_w = outer.width as i32;
    let work_w = work.right - work.left;
    let work_h = work.bottom - work.top;
    let x = work.left + (work_w - win_w) / 2;
    let y = work.top + (work_h as f64 * top_padding_fraction) as i32;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

fn primary_monitor_work_area() -> Option<WorkRect> {
    // `MONITOR_DEFAULTTOPRIMARY` makes the origin point irrelevant — the call
    // resolves to the primary monitor whatever we pass.
    let monitor: HMONITOR =
        unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    monitor_work_area(monitor)
}

struct WorkRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

fn cursor_monitor_work_area() -> Option<WorkRect> {
    let mut cursor = POINT::default();
    let ok = unsafe { GetCursorPos(&mut cursor).is_ok() };
    if !ok {
        return None;
    }
    let monitor: HMONITOR = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
    monitor_work_area(monitor)
}

fn monitor_work_area(monitor: HMONITOR) -> Option<WorkRect> {
    if monitor.is_invalid() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() };
    if !ok {
        return None;
    }
    Some(WorkRect {
        left: info.rcWork.left,
        top: info.rcWork.top,
        right: info.rcWork.right,
        bottom: info.rcWork.bottom,
    })
}
