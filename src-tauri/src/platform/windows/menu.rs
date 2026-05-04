//! Native Win32 popup menu for the system-tray icon on Windows.
//!
//! Why native and not the Tauri webview popup we use on macOS: a non-
//! activating webview window (`WS_EX_NOACTIVATE`) has no reliable
//! outside-click dismiss mechanism on Windows. `WindowEvent::Focused(false)`
//! never fires (window never has focus to lose); foreground-window polling
//! can't distinguish "user clicked back into the same app they were already
//! in" from "user did nothing"; `WH_MOUSE_LL` is silently blocked by EDR/AV
//! on unsigned binaries. Native `TrackPopupMenuEx` lets the OS own the entire
//! interaction — show, hover, dismiss-on-outside-click, dismiss-on-Escape,
//! dismiss-on-alt-tab — so dismissal "just works" no matter the user's
//! security posture.
//!
//! Architecture summary:
//! - A hidden helper window (`HELPER_HWND`, lazily registered) is the menu's
//!   owner. `TrackPopupMenuEx` requires an owner HWND in the calling
//!   thread; the owner doesn't need to be visible.
//! - Per the canonical MSDN tray-icon-menu recipe, we `SetForegroundWindow`
//!   the helper before tracking and `PostMessage(WM_NULL)` after — the latter
//!   is a documented workaround for a bug where the menu wouldn't dismiss
//!   on outside clicks.
//! - We use `TPM_RETURNCMD` so `TrackPopupMenuEx` returns the chosen item ID
//!   directly (no `WM_COMMAND` plumbing through a window proc).

use crate::app::context::AppContext;
use crate::app::FireService;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use tauri::{AppHandle, Manager};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, PostMessageW,
    RegisterClassW, SetForegroundWindow, TrackPopupMenuEx, HMENU, MF_CHECKED, MF_SEPARATOR,
    MF_STRING, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
    WM_NULL, WNDCLASSW, WS_OVERLAPPED,
};

/// Static command IDs returned by `TrackPopupMenuEx`. Static items occupy the
/// 100-band; pinned-prompt items start at `ID_PINNED_BASE` and are indexed by
/// position in the snapshot vec.
const ID_TOGGLE_ARMED: u32 = 100;
const ID_PROMPT_LIBRARY: u32 = 101;
const ID_COMMAND_PALETTE: u32 = 102;
const ID_ABOUT: u32 = 103;
const ID_QUIT: u32 = 104;
const ID_PINNED_BASE: u32 = 1000;

/// Cached helper-window HWND. Win32 `HWND` isn't `Send`, but the raw
/// pointer-as-isize is — and we only ever consume it back into an HWND on
/// the UI thread that created it.
static HELPER_HWND_RAW: AtomicIsize = AtomicIsize::new(0);

/// Re-entrancy guard. `TrackPopupMenuEx` blocks but pumps messages; without
/// this, a tray-icon click that dismisses the current menu can dispatch a
/// fresh tray-click event → fresh menu → recursion.
static MENU_TRACKING: AtomicBool = AtomicBool::new(false);

/// Lazily register and create the hidden owner window. Idempotent.
unsafe fn helper_hwnd() -> HWND {
    let cached = HELPER_HWND_RAW.load(Ordering::Acquire);
    if cached != 0 {
        return HWND(cached as _);
    }
    let hinstance = GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default();
    let class_name = w!("PromptPlayerMenuOwner");
    // `WNDCLASSW.lpfnWndProc` wants `Option<unsafe extern "system" fn(...)>`;
    // `DefWindowProcW` from windows-rs is plain `unsafe fn(...)` so we need a
    // thin `extern "system"` trampoline.
    let class = WNDCLASSW {
        lpfnWndProc: Some(def_window_proc_trampoline),
        hInstance: hinstance,
        lpszClassName: class_name,
        ..Default::default()
    };
    // RegisterClassW returns 0 on failure but also if the class already
    // exists. We treat both as fine — the second-registration error doesn't
    // prevent CreateWindowExW from finding the class.
    let _ = RegisterClassW(&class);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class_name,
        w!(""),
        WS_OVERLAPPED, // not WS_VISIBLE — stays hidden
        0,
        0,
        0,
        0,
        None,
        None,
        hinstance,
        None,
    )
    .unwrap_or_default();
    HELPER_HWND_RAW.store(hwnd.0 as isize, Ordering::Release);
    hwnd
}

/// Show the tray menu at the given tray-icon rect and dispatch the chosen
/// action. Blocks the calling thread (`TrackPopupMenuEx` is modal) but
/// pumps messages. Re-entrant calls (e.g. another tray click while the menu
/// is up) are dropped.
pub fn show_tray_menu(app: &AppHandle, rect: tauri::Rect) {
    if MENU_TRACKING.swap(true, Ordering::AcqRel) {
        return;
    }
    let outcome = unsafe { run_menu(app, rect) };
    MENU_TRACKING.store(false, Ordering::Release);
    if let Some((cmd_id, pinned_ids)) = outcome {
        dispatch(app, cmd_id, &pinned_ids);
    }
}

unsafe fn run_menu(app: &AppHandle, rect: tauri::Rect) -> Option<(u32, Vec<String>)> {
    let owner = helper_hwnd();
    if owner.is_invalid() {
        tracing::error!("tray menu: helper window unavailable");
        return None;
    }

    let menu: HMENU = match CreatePopupMenu() {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("CreatePopupMenu failed: {}", e);
            return None;
        }
    };

    // Snapshot state once. The user can't modify state while the menu is up
    // (TrackPopupMenuEx is modal), so snapshotting eliminates the chance of
    // a mid-build inconsistency.
    let ctx = match app.try_state::<AppContext>() {
        Some(s) => s,
        None => {
            let _ = DestroyMenu(menu);
            tracing::warn!("tray menu: AppContext not yet registered");
            return None;
        }
    };
    let armed = ctx.state.is_armed();
    let prompts = ctx.prompts.snapshot();
    let pinned: Vec<&crate::prompts::Prompt> = prompts.iter().filter(|p| p.pinned).collect();

    // 1. Armed toggle (with checkmark when on).
    let toggle_label = wstr("Prompt Player");
    let toggle_flags = if armed {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    };
    let _ = AppendMenuW(
        menu,
        toggle_flags,
        ID_TOGGLE_ARMED as usize,
        PCWSTR(toggle_label.as_ptr()),
    );

    // 2. Pinned prompts (if any).
    let mut pinned_ids: Vec<String> = Vec::with_capacity(pinned.len());
    if !pinned.is_empty() {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        for (i, p) in pinned.iter().enumerate() {
            let id = ID_PINNED_BASE + i as u32;
            let trigger = p.triggers.first().cloned().unwrap_or_default();
            let label = format!("{}\t{}{}", p.name, trigger, p.commit_char);
            let wlabel = wstr(&label);
            let _ = AppendMenuW(menu, MF_STRING, id as usize, PCWSTR(wlabel.as_ptr()));
            pinned_ids.push(p.id.clone());
        }
    }

    // 3. App actions.
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let lib = wstr("Prompt Library");
    let _ = AppendMenuW(
        menu,
        MF_STRING,
        ID_PROMPT_LIBRARY as usize,
        PCWSTR(lib.as_ptr()),
    );
    let cp = wstr("Command Palette…\tCtrl+Alt+\\");
    let _ = AppendMenuW(
        menu,
        MF_STRING,
        ID_COMMAND_PALETTE as usize,
        PCWSTR(cp.as_ptr()),
    );

    // 4. About + Quit.
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let about = wstr("About Prompt Player");
    let _ = AppendMenuW(menu, MF_STRING, ID_ABOUT as usize, PCWSTR(about.as_ptr()));
    let quit = wstr("Quit\tCtrl+Q");
    let _ = AppendMenuW(menu, MF_STRING, ID_QUIT as usize, PCWSTR(quit.as_ptr()));

    // Anchor the menu to the tray icon's top-left in physical pixels.
    // TPM_BOTTOMALIGN places menu.bottom at y, so passing icon.top means
    // the menu grows upward from the icon — correct for the default
    // bottom-edge taskbar. For top/left/right taskbars the OS will adjust
    // automatically to keep the menu on-screen.
    let (x, y) = anchor_xy(&rect);

    // MSDN tray-menu recipe: SetForegroundWindow the owner before tracking,
    // PostMessage(WM_NULL) after. Without these, the menu fails to dismiss
    // on outside clicks — a documented Windows quirk for tray-style menus.
    let _ = SetForegroundWindow(owner);

    let cmd = TrackPopupMenuEx(
        menu,
        TPM_RETURNCMD.0 | TPM_RIGHTBUTTON.0 | TPM_BOTTOMALIGN.0 | TPM_LEFTALIGN.0,
        x,
        y,
        owner,
        None,
    );

    let _ = PostMessageW(owner, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    let cmd_id = cmd.0 as u32;
    if cmd_id == 0 {
        // 0 = user dismissed without selecting (clicked outside, Esc, etc.).
        return None;
    }
    Some((cmd_id, pinned_ids))
}

fn anchor_xy(rect: &tauri::Rect) -> (i32, i32) {
    match rect.position {
        tauri::Position::Physical(p) => (p.x, p.y),
        tauri::Position::Logical(l) => (l.x as i32, l.y as i32),
    }
}

fn dispatch(app: &AppHandle, cmd_id: u32, pinned_ids: &[String]) {
    match cmd_id {
        ID_TOGGLE_ARMED => {
            if let Some(ctx) = app.try_state::<AppContext>() {
                let _ = ctx.state.toggle_armed();
            }
        }
        ID_PROMPT_LIBRARY => show_window(app, "library"),
        ID_COMMAND_PALETTE => {
            if let Some(ctx) = app.try_state::<AppContext>() {
                ctx.focus.capture();
                ctx.search
                    .lock()
                    .rebuild_if_stale(ctx.prompts.generation(), &ctx.prompts.read());
                crate::platform::windows::position_picker_on_cursor_screen(app);
                crate::commands::picker::show_picker_window(app);
            }
        }
        ID_ABOUT => show_window(app, "about"),
        ID_QUIT => app.exit(0),
        id if id >= ID_PINNED_BASE => {
            let idx = (id - ID_PINNED_BASE) as usize;
            if let Some(prompt_id) = pinned_ids.get(idx) {
                if let Some(ctx) = app.try_state::<AppContext>() {
                    let fire = FireService::new(ctx.inner().clone(), app.clone());
                    fire.fire_from_picker(prompt_id, crate::app::fire::PickMode::Human);
                }
            }
        }
        _ => {}
    }
}

fn show_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// UTF-16 + null terminator buffer for `PCWSTR`. The buffer must outlive the
/// Win32 call that consumes it; menu text strings are copied internally by
/// `AppendMenuW`, so each `wstr` can drop after its append.
fn wstr(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Trampoline so we can put `DefWindowProcW` into the `extern "system"` slot
/// in `WNDCLASSW.lpfnWndProc`. The windows-rs binding for `DefWindowProcW`
/// is a plain `unsafe fn`, not `extern "system"`, so we need a thin layer
/// to give it the right ABI.
unsafe extern "system" fn def_window_proc_trampoline(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
