//! Native `WH_KEYBOARD_LL` hook mirroring the macOS tap. Ours rather than
//! `rdev`'s, which hides `LLKHF_INJECTED` and let playback self-cancel.
//!
//! `ToUnicodeEx` against the foreground layout gives chars without consuming
//! dead keys; modifiers use `GetKeyState`, which works off-thread.

use crate::hook::{process_event, HookCallbacks, HookDecision, HookDeps, KeyEvent};
use crate::matcher::MatcherState;
use crate::state::AppState;
use crate::undo::UndoLog;
use std::sync::{Arc, OnceLock};
use std::thread;
use windows::Win32::Foundation::{HMODULE, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, ToUnicodeEx, HKL, VIRTUAL_KEY, VK_BACK, VK_CAPITAL, VK_CONTROL,
    VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RETURN,
    VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_SYSKEYDOWN,
};

/// Per-process hook context. Set once on `spawn`; the extern hook proc reads
/// it via this `OnceLock` because it can't capture closures.
struct HookContext {
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    cb: HookCallbacks,
}

static GLOBAL_CTX: OnceLock<HookContext> = OnceLock::new();

pub fn spawn(
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    cb: HookCallbacks,
) {
    let ctx = HookContext {
        matcher,
        undo,
        app_state: app_state.clone(),
        cb,
    };
    if GLOBAL_CTX.set(ctx).is_err() {
        tracing::warn!("hook::windows::spawn called twice — ignoring duplicate");
        return;
    }

    let app_state_for_thread = app_state;
    thread::Builder::new()
        .name("prompt-player-hook".into())
        .spawn(move || run_hook_thread(app_state_for_thread))
        .expect("spawn hook thread");
}

fn run_hook_thread(app_state: Arc<AppState>) {
    tracing::info!("hook thread starting (native WH_KEYBOARD_LL)");
    // Optimistic; flipped back on install failure, and the setup.rs watcher
    // picks up anything else on its next tick.
    app_state.set_hook_alive(true);

    let hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(hook_proc),
            HMODULE(std::ptr::null_mut()),
            0,
        )
    };
    let hook = match hook {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("SetWindowsHookExW(WH_KEYBOARD_LL) failed: {}", e);
            app_state.set_hook_alive(false);
            return;
        }
    };

    // The OS dispatches to `hook_proc` itself; the pump just keeps this thread
    // alive so the hook stays installed.
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // No TranslateMessage / DispatchMessage — this thread has no
            // window of its own, only the hook.
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

    let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);

    // CRITICAL: drop injected events. Ours would feed the panic ring and
    // self-cancel; other tools' would trigger prompts. macOS filters by PID.
    if info.flags.0 & LLKHF_INJECTED.0 != 0 {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }

    let Some(ctx) = GLOBAL_CTX.get() else {
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
