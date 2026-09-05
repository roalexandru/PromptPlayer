//! Native Windows `WH_KEYBOARD_LL` hook — mirrors the macOS `CGEventTap`
//! architecture. `rdev` doesn't expose `LLKHF_INJECTED` on its `Event` API,
//! which means we couldn't tell apart organic keystrokes from chars our own
//! `SendInput` injects during playback. The result was a feedback loop: our
//! body chars fed the §2.6 panic ring (3 keys / 300 ms) and self-cancelled
//! after ~3 chars.
//!
//! What this module does instead: install our own low-level keyboard hook,
//! check the `LLKHF_INJECTED` bit on every event, and short-circuit injected
//! events before they ever reach `process_event`. This is the structural
//! equivalent of macOS's PID filter at `hook/macos.rs:189-197`.
//!
//! Char translation uses `ToUnicodeEx` against the foreground window's
//! keyboard layout (per-thread layouts are a thing on Windows), with the
//! "don't change keyboard state" flag so dead keys don't get consumed by
//! our hook ahead of the target app. Modifier state is read with
//! `GetKeyState` rather than `GetKeyboardState` because the latter returns
//! stale data when called from a non-foreground thread.

use crate::hook::{process_event, HookDecision, HookDeps, KeyEvent};
use crate::matcher::MatcherState;
use crate::state::AppState;
use crate::undo::UndoLog;
use std::sync::{Arc, OnceLock};
use std::thread;
use windows::Win32::Foundation::{HMODULE, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, ToUnicodeEx, HKL, VIRTUAL_KEY, VK_BACK, VK_CAPITAL, VK_CONTROL,
    VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RETURN, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_SYSKEYDOWN,
};

pub type FireCallback = Arc<dyn Fn(Vec<crate::matcher::PromptId>, String) + Send + Sync>;
pub type UndoCallback = Arc<dyn Fn() + Send + Sync>;
pub type LiteralCommitCallback = Arc<dyn Fn(char) + Send + Sync>;
pub type CommitObservedCallback = Arc<dyn Fn(bool, usize) + Send + Sync>;

/// Per-process hook context. Set once on `spawn`; the extern hook proc reads
/// it via this `OnceLock` because it can't capture closures.
struct HookContext {
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    on_fire: FireCallback,
    on_undo: UndoCallback,
    on_literal_commit: LiteralCommitCallback,
    on_commit_observed: CommitObservedCallback,
}

static GLOBAL_CTX: OnceLock<HookContext> = OnceLock::new();

pub fn spawn(
    matcher: Arc<MatcherState>,
    undo: Arc<UndoLog>,
    app_state: Arc<AppState>,
    on_fire: FireCallback,
    on_undo: UndoCallback,
    on_literal_commit: LiteralCommitCallback,
    on_commit_observed: CommitObservedCallback,
) {
    let ctx = HookContext {
        matcher,
        undo,
        app_state: app_state.clone(),
        on_fire,
        on_undo,
        on_literal_commit,
        on_commit_observed,
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
    // Mark alive optimistically. If install fails below we flip back to
    // false; if anything else surprises us, the watcher in setup.rs picks
    // up the false state on its next tick.
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
        Ok(h) => {
            tracing::info!(
                hhook = h.0 as usize,
                "WH_KEYBOARD_LL installed"
            );
            h
        }
        Err(e) => {
            tracing::error!("SetWindowsHookExW(WH_KEYBOARD_LL) failed: {}", e);
            app_state.set_hook_alive(false);
            return;
        }
    };

    // Standard message pump. Low-level hooks are dispatched to `hook_proc`
    // automatically by the OS; the pump just keeps the thread alive so the
    // hook stays installed.
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

    // Trace-level raw event log. Gated to `trace` so it costs nothing in
    // release builds with the default `info` filter; flip via
    // `RUST_LOG=prompt_player::hook=trace` when diagnosing "hotkey doesn't
    // fire during Zoom share" (could be Zoom's hook eating events upstream).
    tracing::trace!(
        target: "prompt_player::hook",
        vk = info.vkCode,
        scan = info.scanCode,
        flags = info.flags.0,
        injected = info.flags.0 & LLKHF_INJECTED.0 != 0,
        "raw key event"
    );

    // CRITICAL FILTER: skip injected events. These come from `SendInput`
    // calls — ours during playback, or any other automation tool's. Letting
    // them flow through `process_event` would (a) feed our own body chars
    // into the panic-ring and self-cancel after 3 chars, and (b) let other
    // automation tools accidentally trigger our prompts. macOS filters by
    // PID at `hook/macos.rs:195`; we filter by the OS-tagged flag here.
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
        on_fire: &ctx.on_fire,
        on_undo: &ctx.on_undo,
        on_literal_commit: &ctx.on_literal_commit,
        on_commit_observed: &ctx.on_commit_observed,
    };
    match process_event(&key_event, &deps) {
        HookDecision::Pass => CallNextHookEx(HHOOK::default(), code, wparam, lparam),
        // Suppress the keystroke entirely — non-zero return tells Windows
        // not to dispatch the event to the target window (this is how the
        // commit char gets eaten when a trigger fires).
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

    let typed = if is_backspace || is_pure_modifier || is_separator {
        None
    } else {
        unsafe { translate_to_unicode(info.vkCode, info.scanCode) }
    };

    KeyEvent {
        typed,
        is_backspace,
        is_pure_modifier,
        is_separator,
    }
}

unsafe fn translate_to_unicode(vk_code: u32, scan_code: u32) -> Option<char> {
    // Build a 256-byte key state vector covering the modifiers
    // `ToUnicodeEx` actually consults. `GetKeyState` works from any thread
    // (returns the synchronous virtual key state), unlike `GetKeyboardState`
    // which returns stale data when called from a non-foreground thread —
    // exactly our situation here.
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

    // Use the active layout of the foreground window — Windows is per-thread
    // about layouts, and we want the same layout the user is actually
    // typing into. Falls back to current-thread layout if the foreground
    // window can't be queried.
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
    // `wflags = 4` (bit 2) = "do not change keyboard state" — without this,
    // `ToUnicodeEx` would consume any pending dead-key state, breaking the
    // user's actual composition in the target app. Requires Win10 1607+,
    // which is universal at this point.
    let result = ToUnicodeEx(vk_code, scan_code, &key_state, &mut buf, 4, layout);
    if result <= 0 {
        // 0 = no char produced (modifier alone). -1 = dead key (with the
        // "don't change state" flag we won't consume it; the next press
        // will translate normally).
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..result as usize]);
    s.chars().next().filter(|c| !c.is_control())
}
