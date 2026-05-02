//! macOS activation helpers.
//!
//! `activate_app` brings our process to the foreground (required before
//! showing a regular window when our policy is `.accessory` — without it the
//! shown window gets back-grounded immediately because we have no Dock icon).
//!
//! `order_panel_front_no_activate` shows an NSPanel WITHOUT activating the
//! app — the menu-bar-popover idiom that lets the foreground app retain the
//! key window.

use cocoa::base::{id, nil, YES};
use objc::{class, msg_send, sel, sel_impl};
use tauri::WebviewWindow;

/// Bring our application to the foreground via `[NSApp activateIgnoringOtherApps:YES]`.
///
/// Used by `show_window` for the library window and the picker. With
/// activation policy `.accessory`, this does NOT pull the user out of
/// fullscreen-app Spaces.
pub fn activate_app() {
    unsafe {
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: YES];
    }
}

/// Show an NSPanel WITHOUT activating the app. Use when the panel has the
/// `nonActivating` style mask and we want the foreground app to keep its key
/// window (true menu-bar-popover semantics).
pub fn order_panel_front_no_activate(window: &WebviewWindow) {
    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    unsafe {
        let ns_window: id = ns_window_ptr as id;
        let _: () = msg_send![ns_window, makeKeyAndOrderFront: nil];
    }
}
