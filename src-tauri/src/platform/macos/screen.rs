//! NSScreen geometry helpers.
//!
//! Single source of truth for "find the screen that contains the cursor and
//! place a window centered there, ~30% from the top (Spotlight placement)".
//!
//! All geometry math is in AppKit bottom-left coords; we use `setFrameOrigin:`
//! directly so no conversion to Tauri's top-left coord system is needed.

use cocoa::base::{id, nil};
use cocoa::foundation::{NSPoint, NSRect};
use objc::{class, msg_send, sel, sel_impl};
use tauri::AppHandle;
use tauri::Manager;

/// Find the visible frame of the NSScreen that contains the cursor.
fn cursor_screen_visible_frame() -> Option<NSRect> {
    unsafe {
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let screens: id = msg_send![class!(NSScreen), screens];
        if screens == nil {
            return None;
        }
        let count: usize = msg_send![screens, count];
        for i in 0..count {
            let screen: id = msg_send![screens, objectAtIndex: i];
            let frame: NSRect = msg_send![screen, frame];
            if cursor.x >= frame.origin.x
                && cursor.x <= frame.origin.x + frame.size.width
                && cursor.y >= frame.origin.y
                && cursor.y <= frame.origin.y + frame.size.height
            {
                let vf: NSRect = msg_send![screen, visibleFrame];
                return Some(vf);
            }
        }
        None
    }
}

/// Reposition the picker centered horizontally on the screen that contains
/// the cursor, with `top_padding_fraction` of the visible-frame height
/// between the top of the screen and the top of the window (Spotlight uses
/// ~0.30). Does NOT show or activate.
pub fn position_centered_on_cursor(window: &tauri::WebviewWindow, top_padding_fraction: f64) {
    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    unsafe {
        let ns_window: id = ns_window_ptr as id;
        let Some(vf) = cursor_screen_visible_frame() else {
            return;
        };
        let win_frame: NSRect = msg_send![ns_window, frame];
        let x = vf.origin.x + (vf.size.width - win_frame.size.width) / 2.0;
        let y_top_pad = vf.size.height * top_padding_fraction;
        let y = vf.origin.y + vf.size.height - y_top_pad - win_frame.size.height;
        let _: () = msg_send![ns_window, setFrameOrigin: NSPoint { x, y }];
    }
}

/// Position the picker on whichever screen contains the cursor (Spotlight
/// placement). Caller is expected to invoke this on the main thread —
/// `NSEvent.mouseLocation` and `NSScreen.screens` return stale data when
/// called from a background thread.
pub fn position_picker_on_cursor_screen(app: &AppHandle) {
    let Some(window) = app.get_webview_window("picker") else {
        return;
    };
    position_centered_on_cursor(&window, 0.30);
}

/// True when the desktop spans more than one physical display.
///
/// Mirrored displays are one entry in `NSScreen.screens` — the OS presents a
/// single logical screen — so a count above one means the presenter's screen
/// and the projector show *different* content. That is exactly the case where
/// placing the picker on the cursor's screen can put it on the projector:
/// capture exclusion hides the picker from Zoom, but nothing hides it from a
/// second physical output.
pub fn is_extended_desktop() -> bool {
    unsafe {
        let screens: id = msg_send![class!(NSScreen), screens];
        if screens == nil {
            return false;
        }
        let count: usize = msg_send![screens, count];
        count > 1
    }
}

/// Visible frame of the primary screen — `screens[0]`, the one carrying the
/// menu bar, which is the presenter's own display in every normal setup.
fn primary_screen_visible_frame() -> Option<NSRect> {
    unsafe {
        let screens: id = msg_send![class!(NSScreen), screens];
        if screens == nil {
            return None;
        }
        let count: usize = msg_send![screens, count];
        if count == 0 {
            return None;
        }
        let screen: id = msg_send![screens, objectAtIndex: 0usize];
        let vf: NSRect = msg_send![screen, visibleFrame];
        Some(vf)
    }
}

/// Same placement as `position_centered_on_cursor`, but pinned to the primary
/// display regardless of where the cursor is.
pub fn position_centered_on_primary(window: &tauri::WebviewWindow, top_padding_fraction: f64) {
    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    unsafe {
        let ns_window: id = ns_window_ptr as id;
        let Some(vf) = primary_screen_visible_frame() else {
            return;
        };
        let win_frame: NSRect = msg_send![ns_window, frame];
        let x = vf.origin.x + (vf.size.width - win_frame.size.width) / 2.0;
        let y_top_pad = vf.size.height * top_padding_fraction;
        let y = vf.origin.y + vf.size.height - y_top_pad - win_frame.size.height;
        let _: () = msg_send![ns_window, setFrameOrigin: NSPoint { x, y }];
    }
}

/// Find the visible-frame of the NSScreen containing the cursor. AppKit
/// coords (logical points, bottom-left origin, single unified coord space
/// across all monitors regardless of DPI). Returns None if no screen
/// contains the cursor (rare; can happen during space transitions).
fn cursor_screen_frame_full() -> Option<(NSRect, NSRect)> {
    unsafe {
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let screens: id = msg_send![class!(NSScreen), screens];
        if screens == nil {
            return None;
        }
        let count: usize = msg_send![screens, count];
        for i in 0..count {
            let screen: id = msg_send![screens, objectAtIndex: i];
            let frame: NSRect = msg_send![screen, frame];
            if cursor.x >= frame.origin.x
                && cursor.x <= frame.origin.x + frame.size.width
                && cursor.y >= frame.origin.y
                && cursor.y <= frame.origin.y + frame.size.height
            {
                let visible: NSRect = msg_send![screen, visibleFrame];
                return Some((frame, visible));
            }
        }
        None
    }
}

/// Position the tray popover relative to the cursor (which sits on the
/// tray icon at click time). Uses NSEvent + NSScreen + setFrameOrigin
/// directly — bypasses Tauri's PhysicalPosition path which is ambiguous
/// on mixed-DPI multi-monitor setups (monitor physical-pixel bounds can
/// overlap when scale factors differ).
///
/// Anchor: just below the menu bar, horizontally aligned with the cursor's
/// x. Clamped to the click monitor's visible frame so the popup never
/// crosses a screen edge.
pub fn position_popover_under_cursor(window: &tauri::WebviewWindow) {
    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    unsafe {
        let ns_window: id = ns_window_ptr as id;
        let Some((screen_frame, visible_frame)) = cursor_screen_frame_full() else {
            return;
        };
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let win_frame: NSRect = msg_send![ns_window, frame];

        // Cursor is in AppKit coords (Y-up from the primary screen's
        // bottom-left). visibleFrame.size.height excludes the menu bar
        // and Dock; visibleFrame.origin.y is just below the menu bar.
        // Place the window's TOP edge 4 pt below the menu bar.
        let menu_bar_bottom_y = visible_frame.origin.y + visible_frame.size.height; // top of visible area
        let target_y = menu_bar_bottom_y - 4.0 - win_frame.size.height;
        // Native NSMenu under NSStatusItem is LEFT-aligned with the icon's
        // left edge — not centered under the cursor. The cursor lands
        // approximately in the icon's center, so we shift the popup's
        // left edge ~12 pt left of the cursor (≈ half a 22-pt icon width).
        // Clamping below keeps the popup on the screen if the icon sits
        // near the right edge.
        const ICON_HALF_WIDTH_PT: f64 = 12.0;
        let mut target_x = cursor.x - ICON_HALF_WIDTH_PT;

        // Clamp to the click screen's visible frame.
        let left_edge = visible_frame.origin.x + 4.0;
        let right_edge = visible_frame.origin.x + visible_frame.size.width - 4.0;
        if target_x + win_frame.size.width > right_edge {
            target_x = right_edge - win_frame.size.width;
        }
        if target_x < left_edge {
            target_x = left_edge;
        }
        let _ = screen_frame;

        let origin = NSPoint {
            x: target_x,
            y: target_y,
        };
        let _: () = msg_send![ns_window, setFrameOrigin: origin];
    }
}
