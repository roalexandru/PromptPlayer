//! Native macOS keyboard hook via `CGEventTap`.
//!
//! rdev's macOS path crashes on newer macOS because its `string_from_code`
//! helper calls `TSMGetInputSourceProperty` from the tap callback thread,
//! which violates dispatch-queue assertions and SIGTRAPs.
//!
//! We use `CGEventKeyboardGetUnicodeString` instead — that API reads the
//! Unicode chars from the event directly without going through TSM, and is
//! safe to call from the tap callback queue.

use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPortRef;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop, CFRunLoopRun, CFRunLoopSourceRef};
use core_graphics::event::{
    CGEvent, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
};
use std::os::raw::c_void;
use std::sync::Arc;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: extern "C" fn(
            proxy: *mut c_void,
            etype: u32,
            event: *mut c_void,
            user_info: *mut c_void,
        ) -> *mut c_void,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: i64,
    ) -> CFRunLoopSourceRef;

    fn CGEventKeyboardGetUnicodeString(
        event: *mut c_void,
        max_string_length: u64,
        actual_string_length: *mut u64,
        unicode_string: *mut u16,
    );

    fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;

    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
}

const KCG_SESSION_EVENT_TAP: u32 = 1; // kCGSessionEventTap
const KCG_HEAD_INSERT_EVENT_TAP: u32 = 0; // kCGHeadInsertEventTap
const KCG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
const KCG_EVENT_KEY_DOWN: u32 = 10;
const KCG_EVENT_FLAGS_CHANGED: u32 = 12;
const KCG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFFFFFE;
const KCG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFFFFFD;

/// Virtual key code field of CGEvent.
const KCG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
/// PID of the process that posted the event. For our own enigo-injected events
/// this equals our own PID; for real keyboard input it's WindowServer/0.
const KCG_EVENT_SOURCE_UNIX_PROCESS_ID: u32 = 41;

/// Decision returned to CGEventTap. None = suppress, Some(()) = pass through.
pub type Decision = Option<()>;

/// Per-event callback invoked from the tap thread. Returns Pass or Suppress.
pub type EventHandler = Arc<dyn Fn(KeyEvent) -> Decision + Send + Sync>;

#[derive(Debug, Clone)]
pub struct KeyEvent {
    pub keycode: u16,
    pub typed: Option<char>,
    pub is_backspace: bool,
}

const KEY_CODE_DELETE: u16 = 51; // backspace on US layout

/// Spawn the CGEventTap on a dedicated thread with its own CFRunLoop.
pub fn spawn(handler: EventHandler) {
    std::thread::Builder::new()
        .name("prompt-player-cgevent-tap".into())
        .spawn(move || run_tap_thread(handler))
        .expect("spawn cgevent tap thread");
}

fn run_tap_thread(handler: EventHandler) {
    // Box the handler to a stable address; pass to the C callback via user_info.
    let handler_ptr = Box::into_raw(Box::new(handler)) as *mut c_void;
    let mask = (1u64 << KCG_EVENT_KEY_DOWN) | (1u64 << KCG_EVENT_FLAGS_CHANGED);

    let tap_port = unsafe {
        CGEventTapCreate(
            KCG_SESSION_EVENT_TAP,
            KCG_HEAD_INSERT_EVENT_TAP,
            KCG_EVENT_TAP_OPTION_DEFAULT,
            mask,
            tap_callback,
            handler_ptr,
        )
    };
    if tap_port.is_null() {
        tracing::error!(
            "CGEventTapCreate returned null — Accessibility permission likely missing"
        );
        return;
    }

    let runloop_source =
        unsafe { CFMachPortCreateRunLoopSource(std::ptr::null(), tap_port, 0) };

    let current_loop = CFRunLoop::get_current();
    unsafe {
        let raw_loop = current_loop.as_concrete_TypeRef();
        core_foundation::runloop::CFRunLoopAddSource(
            raw_loop,
            runloop_source,
            kCFRunLoopCommonModes,
        );
    }
    unsafe { CGEventTapEnable(tap_port, true) };

    tracing::info!("CGEventTap installed; entering run loop");
    unsafe { CFRunLoopRun() };
    tracing::warn!("CFRunLoopRun returned — tap thread exiting");

    // Reclaim the boxed handler.
    unsafe { drop(Box::from_raw(handler_ptr as *mut EventHandler)) };
}

extern "C" fn tap_callback(
    _proxy: *mut c_void,
    etype: u32,
    event: *mut c_void,
    user_info: *mut c_void,
) -> *mut c_void {
    // Re-enable on auto-disable.
    if etype == KCG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || etype == KCG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        // We can't re-enable from within the callback safely; log and return.
        tracing::warn!("CGEventTap disabled by system (etype={:#x})", etype);
        return event;
    }
    if etype != KCG_EVENT_KEY_DOWN {
        return event;
    }

    // Suppress our own injected keystrokes — when the typer fires, its events
    // would otherwise bounce back through the tap and look like the user typing,
    // which then cancels playback at the §2.6 3-keystroke threshold.
    let source_pid = unsafe {
        CGEventGetIntegerValueField(event, KCG_EVENT_SOURCE_UNIX_PROCESS_ID)
    } as u32;
    let our_pid = std::process::id();
    if source_pid == our_pid {
        return event;
    }

    let handler = unsafe { &*(user_info as *const EventHandler) };

    // Extract typed Unicode (no TSM — this is dispatch-queue safe).
    let mut buf = [0u16; 8];
    let mut len: u64 = 0;
    unsafe {
        CGEventKeyboardGetUnicodeString(
            event,
            buf.len() as u64,
            &mut len,
            buf.as_mut_ptr(),
        );
    }
    let s = String::from_utf16_lossy(&buf[..len as usize]);
    let typed: Option<char> = s.chars().find(|c| !c.is_control());

    let keycode =
        unsafe { CGEventGetIntegerValueField(event, KCG_KEYBOARD_EVENT_KEYCODE) } as u16;
    let is_backspace = keycode == KEY_CODE_DELETE;

    tracing::debug!(
        "tap event keycode={} typed={:?} is_backspace={}",
        keycode,
        typed,
        is_backspace
    );

    let evt = KeyEvent {
        keycode,
        typed,
        is_backspace,
    };

    match handler(evt) {
        None => std::ptr::null_mut(), // suppress
        Some(()) => event,
    }
}

// Re-export the symbols we use from core-graphics so callers don't need to import them.
pub use core_graphics::event::CGEventTapLocation as _CGEventTapLocation;
pub use core_graphics::event::CGEventTapOptions as _CGEventTapOptions;
pub use core_graphics::event::CGEventTapPlacement as _CGEventTapPlacement;
