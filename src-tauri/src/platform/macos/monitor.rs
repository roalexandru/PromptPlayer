//! NSEvent global / local monitor for the tray popover.
//!
//! Two monitors are installed when the popover is shown:
//! - **Global mouse-down monitor** — dismisses the popover on any click
//!   anywhere outside it (the canonical NSStatusItem-popover behavior).
//! - **Local mouse-moved monitor** — feeds cursor positions to the popover
//!   webview as Tauri events. WKWebView inside a non-activating NSPanel
//!   doesn't dispatch mouseMoved events to JS, so CSS `:hover` and JS
//!   hover state freeze without this workaround.
//!
//! `OutsideClickMonitor` Drop-removes both monitors if they're still
//! installed, so app shutdown doesn't leak them.

use block::ConcreteBlock;
use cocoa::base::{id, nil};
use objc::{class, msg_send, sel, sel_impl};
use parking_lot::Mutex;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

pub struct OutsideClickMonitor {
    outside: Mutex<Option<usize>>,
    mouse_track: Mutex<Option<usize>>,
}

impl OutsideClickMonitor {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self {
            outside: Mutex::new(None),
            mouse_track: Mutex::new(None),
        })
    }

    fn remove_inner(&self) {
        unsafe {
            if let Some(ptr) = self.outside.lock().take() {
                let nsevent_class = class!(NSEvent);
                let _: () = msg_send![nsevent_class, removeMonitor: ptr as id];
            }
            if let Some(ptr) = self.mouse_track.lock().take() {
                let nsevent_class = class!(NSEvent);
                let _: () = msg_send![nsevent_class, removeMonitor: ptr as id];
            }
        }
    }
}

impl Drop for OutsideClickMonitor {
    fn drop(&mut self) {
        self.remove_inner();
    }
}

const NSEVENT_MASK_LEFT_MOUSE_DOWN: u64 = 1 << 1;
const NSEVENT_MASK_RIGHT_MOUSE_DOWN: u64 = 1 << 3;
const NSEVENT_MASK_OTHER_MOUSE_DOWN: u64 = 1 << 25;
const NSEVENT_MASK_MOUSE_MOVED: u64 = 1 << 5;

pub fn install_outside_click_monitor(app: &AppHandle, monitor_state: &Arc<OutsideClickMonitor>) {
    if monitor_state.outside.lock().is_some() {
        return;
    }
    let mask: u64 = NSEVENT_MASK_LEFT_MOUSE_DOWN
        | NSEVENT_MASK_RIGHT_MOUSE_DOWN
        | NSEVENT_MASK_OTHER_MOUSE_DOWN;

    // Global click → hide popover.
    let app_for_click = app.clone();
    let click_block = ConcreteBlock::new(move |_event: id| {
        if let Some(w) = app_for_click.get_webview_window("tray-popup") {
            let _ = w.hide();
        }
    });
    let click_block = click_block.copy();
    unsafe {
        let nsevent_class = class!(NSEvent);
        let monitor: id = msg_send![
            nsevent_class,
            addGlobalMonitorForEventsMatchingMask: mask
            handler: &*click_block
        ];
        if monitor != nil {
            *monitor_state.outside.lock() = Some(monitor as usize);
        }
    }

    // Local mouse-moved → emit Tauri event with CSS coords.
    if monitor_state.mouse_track.lock().is_some() {
        return;
    }
    let app_for_move = app.clone();
    let move_block = ConcreteBlock::new(move |event: id| -> id {
        unsafe {
            if let Some(w) = app_for_move.get_webview_window("tray-popup") {
                if let Ok(ns_window_ptr) = w.ns_window() {
                    let ns_window: id = ns_window_ptr as id;
                    let ev_window: id = msg_send![event, window];
                    let loc: cocoa::foundation::NSPoint = if ev_window == ns_window {
                        msg_send![event, locationInWindow]
                    } else {
                        let screen_loc: cocoa::foundation::NSPoint =
                            msg_send![event, locationInWindow];
                        let global: cocoa::foundation::NSPoint = if ev_window != nil {
                            msg_send![ev_window, convertPointToScreen: screen_loc]
                        } else {
                            screen_loc
                        };
                        let p: cocoa::foundation::NSPoint =
                            msg_send![ns_window, convertPointFromScreen: global];
                        p
                    };
                    let frame: cocoa::foundation::NSRect = msg_send![ns_window, frame];
                    let css_x = loc.x;
                    let css_y = frame.size.height - loc.y;
                    let _ = w.emit("tray-popup-mousemove", (css_x, css_y));
                }
            }
        }
        event
    });
    let move_block = move_block.copy();
    unsafe {
        let nsevent_class = class!(NSEvent);
        let monitor: id = msg_send![
            nsevent_class,
            addLocalMonitorForEventsMatchingMask: NSEVENT_MASK_MOUSE_MOVED
            handler: &*move_block
        ];
        if monitor != nil {
            *monitor_state.mouse_track.lock() = Some(monitor as usize);
        }
    }
}

pub fn remove_outside_click_monitor(monitor_state: &Arc<OutsideClickMonitor>) {
    monitor_state.remove_inner();
}
