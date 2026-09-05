//! `activate_app` foregrounds our process, needed before showing a regular
//! window under `.accessory` (no Dock icon to fall back on).
//! `order_panel_front_no_activate` shows a panel without activating, so the
//! foreground app keeps its key window.

use cocoa::base::{id, nil, YES};
use objc::{class, msg_send, sel, sel_impl};
use tauri::WebviewWindow;

/// `[NSApp activateIgnoringOtherApps:YES]`. Under `.accessory` this does not
/// pull the user out of a fullscreen Space.
pub fn activate_app() {
    unsafe {
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: YES];
    }
}

/// Show a `nonActivating` NSPanel without activating the app — true menu-bar
/// popover semantics, where the foreground app keeps its key window.
pub fn order_panel_front_no_activate(window: &WebviewWindow) {
    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    unsafe {
        let ns_window: id = ns_window_ptr as id;
        let _: () = msg_send![ns_window, makeKeyAndOrderFront: nil];
    }
}
