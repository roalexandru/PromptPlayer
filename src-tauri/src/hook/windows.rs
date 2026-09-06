//! Native `WH_KEYBOARD_LL` hook mirroring the macOS tap. Ours rather than
//! `rdev`'s, which hides `LLKHF_INJECTED` and let playback self-cancel.
//!
//! `ToUnicodeEx` against the foreground layout gives chars without consuming
//! dead keys; modifiers use `GetKeyState`, which works off-thread.

use crate::hook::{process_event, HookCallbacks, HookDecision, HookDeps, KeyEvent};
use crate::matcher::MatcherState;
use crate::state::AppState;
use crate::undo::UndoLog;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use windows::Win32::Foundation::{HMODULE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, ToUnicodeEx, HKL, VIRTUAL_KEY, VK_BACK, VK_CAPITAL, VK_CONTROL,
    VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RETURN,
    VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, PostThreadMessageW,
    SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_SYSKEYDOWN,
};

/// Per-process hook context. The extern hook proc reads it through this static
/// because it can't capture closures. `RwLock<Option<_>>` rather than a
/// `OnceLock` so a reinstall can rebind it — with a `OnceLock`, the second
/// `spawn` returned early and every watchdog repair was a silent no-op.
struct HookContext {
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    cb: HookCallbacks,
}

static GLOBAL_CTX: RwLock<Option<Arc<HookContext>>> = RwLock::new(None);

/// Raw key events the hook proc has seen. Windows silently detaches a
/// low-level hook whose proc overruns `LowLevelHooksTimeout` — no callback, no
/// error, it just stops being called, and `hook_alive` stays `true` forever.
/// This counter is the only evidence that the hook is still in the chain.
static EVENTS_SEEN: AtomicU64 = AtomicU64::new(0);

/// Thread id of the message pump that owns the hook. A low-level hook belongs
/// to the installing thread's queue, so a reinstall has to happen there.
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Posted to the pump thread to ask for an unhook-and-reinstall.
const WM_PP_REINSTALL: u32 = WM_APP + 1;

/// Raw key events observed since process start. The watchdog uses it to tell
/// "the hook is dead" from "nobody is typing".
pub fn events_seen() -> u64 {
    EVENTS_SEEN.load(Ordering::Relaxed)
}

/// Milliseconds since the last system-wide input event of any kind (keyboard
/// or mouse). `None` when the OS won't say.
pub fn idle_millis() -> Option<u32> {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is a correctly sized, initialized out-parameter.
    if unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        // Both are `GetTickCount` values, so wrapping subtraction is correct
        // across the 49.7-day rollover.
        Some(unsafe { GetTickCount() }.wrapping_sub(info.dwTime))
    } else {
        None
    }
}

/// Ask the pump thread to reinstall the hook. Cheap and idempotent: the hook
/// holds no state, so a needless reinstall costs one syscall pair.
pub fn request_reinstall() -> bool {
    let tid = HOOK_THREAD_ID.load(Ordering::Acquire);
    if tid == 0 {
        return false;
    }
    // SAFETY: posting a thread message is safe for any thread id; a stale id
    // makes the call fail rather than misbehave.
    unsafe { PostThreadMessageW(tid, WM_PP_REINSTALL, WPARAM(0), LPARAM(0)) }.is_ok()
}

pub fn spawn(
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    cb: HookCallbacks,
) {
    let ctx = Arc::new(HookContext {
        matcher,
        undo,
        app_state: app_state.clone(),
        cb,
    });
    {
        let mut guard = GLOBAL_CTX.write();
        if guard.is_some() {
            // The pump thread is already running; a reinstall goes through it.
            tracing::debug!("hook already spawned — requesting a reinstall instead");
            drop(guard);
            request_reinstall();
            return;
        }
        *guard = Some(ctx);
    }

    let app_state_for_thread = app_state;
    thread::Builder::new()
        .name("prompt-player-hook".into())
        .spawn(move || run_hook_thread(app_state_for_thread))
        .expect("spawn hook thread");
}

/// Install `WH_KEYBOARD_LL` on the calling thread.
fn install_hook() -> Result<HHOOK, windows::core::Error> {
    // SAFETY: `hook_proc` has the required signature and lives for the process;
    // a null HMODULE is correct for a hook proc inside this process.
    unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(hook_proc),
            HMODULE(std::ptr::null_mut()),
            0,
        )
    }
}

fn run_hook_thread(app_state: Arc<AppState>) {
    tracing::info!("hook thread starting (native WH_KEYBOARD_LL)");
    // SAFETY: no preconditions.
    HOOK_THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::Release);

    let mut hook = match install_hook() {
        Ok(h) => {
            tracing::info!(hhook = h.0 as usize, "WH_KEYBOARD_LL installed");
            app_state.set_hook_alive(true);
            h
        }
        Err(e) => {
            tracing::error!("SetWindowsHookExW(WH_KEYBOARD_LL) failed: {}", e);
            app_state.set_hook_alive(false);
            return;
        }
    };

    // The OS dispatches to `hook_proc` itself; the pump keeps this thread alive
    // so the hook stays installed, and carries reinstall requests from the
    // watchdog. No TranslateMessage / DispatchMessage — this thread has no
    // window of its own, only the hook.
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message != WM_PP_REINSTALL {
                continue;
            }
            let _ = UnhookWindowsHookEx(hook);
            match install_hook() {
                Ok(h) => {
                    hook = h;
                    app_state.set_hook_alive(true);
                    tracing::warn!(
                        hhook = h.0 as usize,
                        "WH_KEYBOARD_LL reinstalled after a silent detach"
                    );
                }
                Err(e) => {
                    app_state.set_hook_alive(false);
                    tracing::error!("WH_KEYBOARD_LL reinstall failed: {}", e);
                    // Keep pumping: the watchdog will ask again.
                }
            }
        }
        let _ = UnhookWindowsHookEx(hook);
    }
    app_state.set_hook_alive(false);
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION as i32 {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }

    // We only care about keydown variants. Keyup leaks state we don't use,
    // and processing it would also double-fire trigger logic.
    let event = wparam.0 as u32;
    if event != WM_KEYDOWN && event != WM_SYSKEYDOWN {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }

    // Bumped before any filtering: this is a liveness signal, not a metric.
    // If it stops advancing while the user is typing, the OS detached us.
    EVENTS_SEEN.fetch_add(1, Ordering::Relaxed);

    let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);

    // Costs nothing at the default `info` filter; flip with
    // `RUST_LOG=prompt_player::hook=trace` to see what the hook actually sees.
    tracing::trace!(
        target: "prompt_player::hook",
        vk = info.vkCode,
        scan = info.scanCode,
        flags = info.flags.0,
        injected = info.flags.0 & LLKHF_INJECTED.0 != 0,
        "raw key event"
    );

    // CRITICAL: drop injected events. Ours would feed the panic ring and
    // self-cancel; other tools' would trigger prompts. macOS filters by PID.
    if info.flags.0 & LLKHF_INJECTED.0 != 0 {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }

    // Clone the Arc and drop the guard immediately: this proc runs under
    // `LowLevelHooksTimeout` (~300ms) and holding a lock across `process_event`
    // is exactly how a hook gets silently detached.
    let Some(ctx) = GLOBAL_CTX.read().clone() else {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    };

    let key_event = translate_kbdllhookstruct(info);
    let deps = HookDeps {
        matcher: &ctx.matcher,
        undo: &ctx.undo,
        app_state: &ctx.app_state,
        cb: &ctx.cb,
        // Windows has no Secure Event Input equivalent.
        secure_input_active: false,
    };
    match process_event(&key_event, &deps) {
        HookDecision::Pass => CallNextHookEx(HHOOK::default(), code, wparam, lparam),
        // Non-zero tells Windows not to dispatch to the target window — this
        // is how the commit char gets eaten when a trigger fires.
        HookDecision::Suppress => LRESULT(1),
    }
}

fn translate_kbdllhookstruct(info: &KBDLLHOOKSTRUCT) -> KeyEvent {
    let vk = VIRTUAL_KEY(info.vkCode as u16);
    let is_backspace = vk == VK_BACK;
    // Return / Tab produce no character but break the word run — surface them
    // as separators so a trigger typed right after Enter still matches.
    let is_separator = vk == VK_RETURN || vk == VK_TAB;

    // Pure-modifier detection — events on Shift / Ctrl / Alt / Win / Caps
    // don't represent typed characters.
    let is_pure_modifier = matches!(
        vk,
        VK_SHIFT
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_CONTROL
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_MENU
            | VK_LMENU
            | VK_RMENU
            | VK_LWIN
            | VK_RWIN
            | VK_CAPITAL
    );

    let is_escape = vk == VK_ESCAPE;

    let typed = if is_backspace || is_pure_modifier || is_separator || is_escape {
        None
    } else {
        unsafe { translate_to_unicode(info.vkCode, info.scanCode) }
    };

    KeyEvent {
        typed,
        is_backspace,
        is_pure_modifier,
        is_separator,
        is_escape,
    }
}

unsafe fn translate_to_unicode(vk_code: u32, scan_code: u32) -> Option<char> {
    // 256-byte state vector for the modifiers `ToUnicodeEx` consults.
    // `GetKeyState` works off the foreground thread; `GetKeyboardState` doesn't.
    let mut key_state = [0u8; 256];
    for vk in [
        VK_SHIFT,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_MENU,
        VK_LMENU,
        VK_RMENU,
        VK_CAPITAL,
    ] {
        let state = GetKeyState(vk.0 as i32) as u16;
        // High bit (0x80) = currently down. Low bit (0x01) = toggle on
        // (for Caps/Num/Scroll). Both encoded in a single byte for ToUnicodeEx.
        key_state[vk.0 as usize] = ((state >> 8) as u8 & 0x80) | ((state & 1) as u8);
    }

    // Layouts are per-thread on Windows, so use the foreground window's.
    // Falls back to this thread's layout if that can't be queried.
    let layout: HKL = {
        let hwnd = GetForegroundWindow();
        if !hwnd.is_invalid() {
            let tid = GetWindowThreadProcessId(hwnd, None);
            GetKeyboardLayout(tid)
        } else {
            GetKeyboardLayout(0)
        }
    };

    let mut buf = [0u16; 8];
    // Bit 2 = "do not change keyboard state", or we'd consume pending dead-key
    // state and break the user's composition. Win10 1607+.
    let result = ToUnicodeEx(vk_code, scan_code, &key_state, &mut buf, 4, layout);
    if result <= 0 {
        // 0 = no char (modifier alone). -1 = dead key, left unconsumed for the
        // next press.
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..result as usize]);
    s.chars().next().filter(|c| !c.is_control())
}
