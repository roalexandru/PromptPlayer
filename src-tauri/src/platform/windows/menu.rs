//! Native Win32 tray menu: a `WS_EX_NOACTIVATE` webview has no reliable
//! outside-click dismiss, so `TrackPopupMenuEx` owns the interaction.
//!
//! A hidden helper window owns it, per the MSDN recipe — foreground before,
//! `WM_NULL` after, `TPM_RETURNCMD` for the id.

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

/// Command IDs from `TrackPopupMenuEx`. Static items use the 100-band; pinned
/// prompts start at `ID_PINNED_BASE`, indexed by snapshot position.
const ID_TOGGLE_ARMED: u32 = 100;
const ID_PROMPT_LIBRARY: u32 = 101;
const ID_COMMAND_PALETTE: u32 = 102;
const ID_ABOUT: u32 = 103;
const ID_QUIT: u32 = 104;
const ID_KEEP_AWAKE: u32 = 105;
const ID_DIAGNOSTICS: u32 = 106;
const ID_HOOK_WARNING: u32 = 107;
const ID_NEXT_CUE: u32 = 108;
const ID_PAUSE_PLAYBACK: u32 = 109;
const ID_RESET_SETLIST: u32 = 110;
const ID_PINNED_BASE: u32 = 1000;

/// Cached helper HWND as an isize: `HWND` isn't `Send`, the raw pointer is,
/// and it's only rebuilt on the UI thread that created it.
static HELPER_HWND_RAW: AtomicIsize = AtomicIsize::new(0);

/// Re-entrancy guard: `TrackPopupMenuEx` blocks but pumps messages, so a
/// dismissing click can otherwise open a fresh menu recursively.
static MENU_TRACKING: AtomicBool = AtomicBool::new(false);

/// Lazily register and create the hidden owner window. Idempotent.
unsafe fn helper_hwnd() -> HWND {
    let cached = HELPER_HWND_RAW.load(Ordering::Acquire);
    if cached != 0 {
        return HWND(cached as _);
    }
    let hinstance = GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default();
    let class_name = w!("PromptPlayerMenuOwner");
    // windows-rs exposes `DefWindowProcW` as a plain `unsafe fn`, so it needs
    // an `extern "system"` trampoline to fit `lpfnWndProc`.
    let class = WNDCLASSW {
        lpfnWndProc: Some(def_window_proc_trampoline),
        hInstance: hinstance,
        lpszClassName: class_name,
        ..Default::default()
    };
    // 0 means failure *or* already-registered; both are fine, since
    // CreateWindowExW still finds the class.
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

/// Show the tray menu at `rect` and dispatch the choice. Modal — blocks the
/// caller while pumping messages; re-entrant calls are dropped.
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

    // Snapshot once — the menu is modal, so nothing can change underneath and
    // a mid-build inconsistency is impossible.
    let ctx = match app.try_state::<AppContext>() {
        Some(s) => s,
        None => {
            let _ = DestroyMenu(menu);
            tracing::warn!("tray menu: AppContext not yet registered");
            return None;
        }
    };
    let armed = ctx.state.is_armed();
    let keep_awake = ctx.power.is_enabled();
    let playing = ctx.state.is_playing();
    let paused = ctx.state.playback_control().is_paused();
    let setlist_len = ctx.config.get().setlist.len();
    let prompts = ctx.prompts.snapshot();
    let pinned: Vec<&crate::prompts::Prompt> = prompts.iter().filter(|p| p.pinned).collect();

    // Capture BEFORE `SetForegroundWindow(owner)` below, or the snapshot is
    // the invisible helper and every later delivery lands on nothing.
    ctx.focus.capture();

    // 0. Hook-health row. macOS has surfaced this in the popover since day
    // one; Windows had no equivalent, so a dead hook there is silent.
    if !ctx.state.hook_alive() {
        let warn = wstr("⚠ Keyboard hook inactive — open Diagnostics");
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            ID_HOOK_WARNING as usize,
            PCWSTR(warn.as_ptr()),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }

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

    // 2. Transport: pause the run in flight, and fire the next setlist cue.
    // Both are only meaningful in context, so neither is shown otherwise —
    // a permanently-greyed row is noise in a menu this small.
    if playing || setlist_len > 0 {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
    if playing {
        let label = wstr(if paused {
            "Resume Typing\tCtrl+Shift+,"
        } else {
            "Pause Typing\tCtrl+Shift+,"
        });
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            ID_PAUSE_PLAYBACK as usize,
            PCWSTR(label.as_ptr()),
        );
    }
    if setlist_len > 0 {
        let cursor = ctx.state.setlist_cursor() % setlist_len;
        let label = wstr(&format!(
            "Next Cue ({} of {})\tCtrl+Shift+.",
            cursor + 1,
            setlist_len
        ));
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            ID_NEXT_CUE as usize,
            PCWSTR(label.as_ptr()),
        );
        let reset = wstr("Rewind Setlist");
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            ID_RESET_SETLIST as usize,
            PCWSTR(reset.as_ptr()),
        );
    }

    // 3. Pinned prompts (if any).
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

    // 4. App actions.
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

    // Keep Awake — checkbox item, labelled with the remaining time so an
    // eight-hour session can't hide behind an anonymous checkmark.
    let ka = wstr(&keep_awake_label(&ctx));
    let ka_flags = if keep_awake {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    };
    let _ = AppendMenuW(menu, ka_flags, ID_KEEP_AWAKE as usize, PCWSTR(ka.as_ptr()));

    // 5. Diagnostics + About + Quit.
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let diag = wstr("Diagnostics…");
    let _ = AppendMenuW(
        menu,
        MF_STRING,
        ID_DIAGNOSTICS as usize,
        PCWSTR(diag.as_ptr()),
    );
    let about = wstr("About Prompt Player");
    let _ = AppendMenuW(menu, MF_STRING, ID_ABOUT as usize, PCWSTR(about.as_ptr()));
    let quit = wstr("Quit\tCtrl+Q");
    let _ = AppendMenuW(menu, MF_STRING, ID_QUIT as usize, PCWSTR(quit.as_ptr()));

    // TPM_BOTTOMALIGN puts menu.bottom at y, so icon.top grows it upward —
    // right for a bottom taskbar, and the OS re-fits the other edges.
    let (x, y) = anchor_xy(&rect);

    // MSDN recipe: foreground the owner before tracking, WM_NULL after, or
    // the menu won't dismiss on outside clicks.
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

/// "Keep Awake", plus the time left when a bounded session is running.
fn keep_awake_label(ctx: &AppContext) -> String {
    match ctx.power.remaining() {
        Some(left) => {
            let mins = (left.as_secs() + 59) / 60;
            if mins >= 60 {
                format!("Keep Awake\t{}h {}m left", mins / 60, mins % 60)
            } else {
                format!("Keep Awake\t{}m left", mins)
            }
        }
        None if ctx.power.is_enabled() => "Keep Awake\tno limit".to_string(),
        None => "Keep Awake".to_string(),
    }
}

fn dispatch(app: &AppHandle, cmd_id: u32, pinned_ids: &[String]) {
    match cmd_id {
        ID_TOGGLE_ARMED => {
            if let Some(ctx) = app.try_state::<AppContext>() {
                // Was a bare `toggle_armed()`, so the Windows tray was the one
                // arm path that neither persisted nor reported.
                let new = !ctx.state.is_armed();
                crate::app::shortcuts::set_armed_and_report(app, &ctx, new);
            }
        }
        ID_PROMPT_LIBRARY => show_window(app, "library"),
        ID_COMMAND_PALETTE => {
            if let Some(ctx) = app.try_state::<AppContext>() {
                // Focus was already captured in `run_menu`, before the helper
                // window took foreground — re-capturing would snapshot it.
                crate::commands::picker::summon_picker(
                    app,
                    &ctx,
                    crate::telemetry::PickerSource::TrayMenu,
                    crate::commands::picker::FocusCapture::AlreadyTaken,
                );
            }
        }
        ID_ABOUT => show_window(app, "about"),
        ID_DIAGNOSTICS | ID_HOOK_WARNING => {
            if let Some(ctx) = app.try_state::<AppContext>() {
                ctx.settings.update(|s| s.setup_seen = true);
                crate::telemetry::send(app, crate::telemetry::TelemetryEvent::DiagnosticsOpened);
            }
            show_window(app, "diagnostics");
        }
        ID_QUIT => app.exit(0),
        ID_KEEP_AWAKE => {
            if let Some(ctx) = app.try_state::<AppContext>() {
                let mins = ctx.settings.get().keep_awake_mins;
                let enabled = ctx.power.toggle_for(mins);
                ctx.settings.update(|s| s.keep_awake = enabled);
                crate::telemetry::send(
                    app,
                    crate::telemetry::TelemetryEvent::KeepAwakeToggled {
                        enabled,
                        duration_mins: if enabled { mins } else { 0 },
                    },
                );
            }
        }
        ID_PAUSE_PLAYBACK => {
            if let Some(ctx) = app.try_state::<AppContext>() {
                match ctx.state.toggle_pause() {
                    Some(true) => tracing::info!("playback PAUSED from tray"),
                    Some(false) => tracing::info!("playback RESUMED from tray"),
                    None => {}
                }
            }
        }
        ID_NEXT_CUE => {
            if let Some(ctx) = app.try_state::<AppContext>() {
                // Same focus caveat as the pinned-prompt path: the hidden
                // helper window holds the foreground after TrackPopupMenuEx,
                // so restore the app captured in `run_menu` before typing.
                let ctx_owned = ctx.inner().clone();
                let app_owned = app.clone();
                std::thread::Builder::new()
                    .name("prompt-player-tray-cue".into())
                    .spawn(move || {
                        if !ctx_owned
                            .focus
                            .restore_and_wait(crate::picker::RESTORATION_TIMEOUT)
                        {
                            std::thread::sleep(crate::picker::RESTORATION_DELAY);
                        }
                        match crate::commands::config::fire_next_cue_inner(&app_owned, &ctx_owned) {
                            Ok(Some(id)) => tracing::info!("tray fired setlist cue → {}", id),
                            Ok(None) => tracing::info!("tray next-cue: setlist is empty"),
                            Err(e) => tracing::warn!("tray next-cue failed: {}", e),
                        }
                    })
                    .expect("spawn tray-cue thread");
            }
        }
        ID_RESET_SETLIST => {
            if let Some(ctx) = app.try_state::<AppContext>() {
                ctx.state.set_setlist_cursor(0);
            }
        }
        id if id >= ID_PINNED_BASE => {
            let idx = (id - ID_PINNED_BASE) as usize;
            if let Some(prompt_id) = pinned_ids.get(idx) {
                if let Some(ctx) = app.try_state::<AppContext>() {
                    // The helper holds the foreground now, so restore the app
                    // captured in `run_menu` and wait, off the event loop.
                    let ctx_owned = ctx.inner().clone();
                    let app_owned = app.clone();
                    let prompt_id = prompt_id.clone();
                    std::thread::Builder::new()
                        .name("prompt-player-tray-fire".into())
                        .spawn(move || {
                            if !ctx_owned
                                .focus
                                .restore_and_wait(crate::picker::RESTORATION_TIMEOUT)
                            {
                                std::thread::sleep(crate::picker::RESTORATION_DELAY);
                            }
                            let fire = FireService::new(ctx_owned, app_owned);
                            fire.fire_from_tray(&prompt_id, crate::app::fire::PickMode::Human);
                        })
                        .expect("spawn tray-fire thread");
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

/// UTF-16 + NUL buffer for `PCWSTR`. `AppendMenuW` copies the text, so each
/// buffer can drop right after its append.
fn wstr(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// ABI trampoline: windows-rs types `DefWindowProcW` as a plain `unsafe fn`,
/// but `lpfnWndProc` needs `extern "system"`.
unsafe extern "system" fn def_window_proc_trampoline(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
