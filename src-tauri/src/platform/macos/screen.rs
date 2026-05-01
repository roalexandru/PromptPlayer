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
