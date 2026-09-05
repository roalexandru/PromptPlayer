//! Native macOS keyboard hook via `CGEventTap`. rdev SIGTRAPs here because its
//! `string_from_code` calls TSM from the tap callback thread; we read chars with
//! `CGEventKeyboardGetUnicodeString`, which is safe on that queue.

use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPortRef;
use core_foundation::runloop::{
    kCFRunLoopCommonModes, CFRunLoop, CFRunLoopRun, CFRunLoopSourceRef,
};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
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
pub type EventHandler = Arc<dyn Fn(NativeKeyEvent) -> Decision + Send + Sync>;

struct TapContext {
    handler: EventHandler,
    status: Arc<crate::state::AppState>,
    tap_port: AtomicPtr<c_void>,
}

/// Native macOS key event before translation into the cross-platform
/// `crate::hook::KeyEvent`.
#[derive(Debug, Clone)]
pub struct NativeKeyEvent {
    pub keycode: u16,
    pub typed: Option<char>,
    pub is_backspace: bool,
}

const KEY_CODE_DELETE: u16 = 51; // backspace on US layout

/// Spawn the CGEventTap on its own thread and CFRunLoop. `status` tracks
/// liveness; the return value says whether the install itself succeeded.
pub fn spawn(handler: EventHandler, status: std::sync::Arc<crate::state::AppState>) -> bool {
    // CGEventTapCreate takes 3-5s on denial (the system shows a dialog), so
    // skip it when we already know permission is missing.
    if !crate::tcc::is_accessibility_trusted() {
        tracing::warn!("Accessibility not trusted — CGEventTap install skipped");
        status.set_hook_alive(false);
        return false;
    }
    let status_for_thread = status.clone();
    std::thread::Builder::new()
        .name("prompt-player-cgevent-tap".into())
        .spawn(move || run_tap_thread(handler, status_for_thread))
        .expect("spawn cgevent tap thread");
    // Install is async on the spawned thread, so the spawn itself counts as
    // success; the watcher respawns if `hook_alive` never flips.
    true
}

fn run_tap_thread(handler: EventHandler, status: std::sync::Arc<crate::state::AppState>) {
    // Box the callback context to a stable address; pass to C via user_info.
    let ctx_ptr = Box::into_raw(Box::new(TapContext {
        handler,
        status: status.clone(),
        tap_port: AtomicPtr::new(std::ptr::null_mut()),
    }));
    let user_info = ctx_ptr as *mut c_void;
    let mask = (1u64 << KCG_EVENT_KEY_DOWN) | (1u64 << KCG_EVENT_FLAGS_CHANGED);

    let tap_port = unsafe {
        CGEventTapCreate(
            KCG_SESSION_EVENT_TAP,
            KCG_HEAD_INSERT_EVENT_TAP,
            KCG_EVENT_TAP_OPTION_DEFAULT,
            mask,
            tap_callback,
            user_info,
        )
    };
    if tap_port.is_null() {
        // ERROR so it clears any `info` filter in Console.app. Accessibility is
        // the common cause, but MDM and sandboxing land here too — say so.
        tracing::error!(
            "CGEventTapCreate returned null. Likely causes: \
             (1) Accessibility permission denied, \
             (2) endpoint-protection software (e.g. JamfProtect) intercepting \
             low-level event taps, \
             (3) stale TCC entry (try `tccutil reset Accessibility com.roalexandru.promptplayer` and re-toggle)."
        );
        status.set_hook_alive(false);
        // Reclaim the boxed handler since no run loop will own it.
        unsafe { drop(Box::from_raw(ctx_ptr)) };
        return;
    }
    unsafe {
        (*ctx_ptr)
            .tap_port
            .store(tap_port as *mut c_void, Ordering::Release);
    }

    let runloop_source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null(), tap_port, 0) };

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
    status.set_hook_alive(true);
    unsafe { CFRunLoopRun() };
    tracing::warn!("CFRunLoopRun returned — tap thread exiting");
    status.set_hook_alive(false);

    // Reclaim the boxed handler.
    unsafe { drop(Box::from_raw(ctx_ptr)) };
}

extern "C" fn tap_callback(
    _proxy: *mut c_void,
    etype: u32,
    event: *mut c_void,
    user_info: *mut c_void,
) -> *mut c_void {
    // Re-enable on auto-disable.
    if etype == KCG_EVENT_TAP_DISABLED_BY_TIMEOUT || etype == KCG_EVENT_TAP_DISABLED_BY_USER_INPUT {
        tracing::warn!("CGEventTap disabled by system (etype={:#x})", etype);
        let ctx = unsafe { &*(user_info as *const TapContext) };
        ctx.status.set_hook_alive(false);
        let tap = ctx.tap_port.load(Ordering::Acquire);
        if !tap.is_null() {
            unsafe { CGEventTapEnable(tap as CFMachPortRef, true) };
            ctx.status.set_hook_alive(true);
            tracing::info!("CGEventTap re-enabled after disable event");
        }
        return event;
    }
    if etype != KCG_EVENT_KEY_DOWN {
        return event;
    }

    // Drop our own injected keys, or playback looks like user typing and
    // self-cancels at the §2.6 threshold.
    let source_pid =
        unsafe { CGEventGetIntegerValueField(event, KCG_EVENT_SOURCE_UNIX_PROCESS_ID) } as u32;
    let our_pid = std::process::id();
    if source_pid == our_pid {
        return event;
    }

    let ctx = unsafe { &*(user_info as *const TapContext) };
    let handler = &ctx.handler;

    // Extract typed Unicode (no TSM — this is dispatch-queue safe).
    let mut buf = [0u16; 8];
    let mut len: u64 = 0;
    unsafe {
        CGEventKeyboardGetUnicodeString(event, buf.len() as u64, &mut len, buf.as_mut_ptr());
    }
    let s = String::from_utf16_lossy(&buf[..len as usize]);
    let typed: Option<char> = s.chars().find(|c| !c.is_control());

    let keycode = unsafe { CGEventGetIntegerValueField(event, KCG_KEYBOARD_EVENT_KEYCODE) } as u16;
    let is_backspace = keycode == KEY_CODE_DELETE;

    tracing::debug!(
        "tap event keycode={} typed={:?} is_backspace={}",
        keycode,
        typed,
        is_backspace
    );

    let evt = NativeKeyEvent {
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
