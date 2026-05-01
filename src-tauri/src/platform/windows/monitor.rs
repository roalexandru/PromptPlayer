//! Windows outside-click monitor — the WH_MOUSE_LL counterpart to the macOS
//! `NSEvent` global mouse-down monitor.
//!
//! Why we need this: with `WS_EX_NOACTIVATE` on the tray popup (see
//! `panel::configure_popover_window`), the popup never becomes the
//! foreground window, so Tauri's `WindowEvent::Focused(false)` handler in
//! `app/lifecycle.rs` is dead code on Windows. Instead, we install a
//! low-level mouse hook that watches for mouse-down anywhere in the system
//! and hides the popup when the click is outside its rect.
//!
//! Threading model:
//! - We spawn a dedicated thread on `install_outside_click_monitor`.
//! - The thread calls `SetWindowsHookExW(WH_MOUSE_LL, ...)` and runs a
//!   `GetMessageW` loop. WH_MOUSE_LL hooks fire on whichever thread's
//!   message pump dispatches the event; by giving it its own thread we
//!   avoid blocking the Tauri main thread or the keyboard hook.
//! - The hook proc cannot directly call `window.hide()` (it'd run on the
//!   hook thread, and Tauri requires UI calls from the main thread). It
//!   posts a request via `AppHandle::run_on_main_thread`, which marshals to
//!   Tauri's event loop.
//! - On `remove_outside_click_monitor` we post `WM_QUIT` to the hook thread,
//!   which causes `GetMessageW` to return 0; the thread then unhooks and
//!   exits cleanly.

use parking_lot::Mutex;
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::{HMODULE, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, GetWindowRect, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HHOOK, MSG, MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_NCLBUTTONDOWN, WM_QUIT, WM_RBUTTONDOWN,
};

/// Per-process state shared between the hook thread and the rest of the app.
/// `OutsideClickMonitor::shared()` returns an `Arc` we register as Tauri
/// managed state; the hook thread reads from a static `MONITOR_GLOBAL` (set
/// during install, cleared during remove) because extern "C" hook procs
/// can't capture closures.
pub struct OutsideClickMonitor {
    inner: Mutex<Option<HookHandle>>,
}

struct HookHandle {
    thread_id: u32,
    // We hold the join handle to keep the thread joinable; we don't actually
    // join (the thread exits on WM_QUIT). Keeping the handle alive prevents
    // the OS from reaping the thread before the hook unhook completes.
    _join: Option<JoinHandle<()>>,
}

impl OutsideClickMonitor {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(None),
        })
    }
}

impl Drop for OutsideClickMonitor {
    fn drop(&mut self) {
        // Best-effort teardown — the actual remove path runs through
        // `remove_outside_click_monitor`. This Drop catches the case where
        // the app exits with the popup still up.
        if let Some(handle) = self.inner.lock().take() {
            quit_hook_thread(&handle);
        }
    }
}

/// Singleton holder for the AppHandle the hook proc needs. Set on install,
/// cleared on remove. Synchronized via `OnceLock<Mutex<...>>` so multiple
/// install/remove cycles work.
static MONITOR_GLOBAL: OnceLock<Mutex<Option<AppHandle>>> = OnceLock::new();

fn global_slot() -> &'static Mutex<Option<AppHandle>> {
    MONITOR_GLOBAL.get_or_init(|| Mutex::new(None))
}

pub fn install_outside_click_monitor(app: &AppHandle, monitor: &Arc<OutsideClickMonitor>) {
    if monitor.inner.lock().is_some() {
        return; // already installed
    }
    *global_slot().lock() = Some(app.clone());

    let (tid_tx, tid_rx) = std::sync::mpsc::channel::<u32>();
    let join = thread::Builder::new()
        .name("promptplayer-mouse-hook".into())
        .spawn(move || run_hook_thread(tid_tx))
        .expect("spawn mouse-hook thread");

    let thread_id = tid_rx.recv().unwrap_or(0);
    *monitor.inner.lock() = Some(HookHandle {
        thread_id,
        _join: Some(join),
    });
}

pub fn remove_outside_click_monitor(monitor: &Arc<OutsideClickMonitor>) {
    let Some(handle) = monitor.inner.lock().take() else {
        return;
    };
    quit_hook_thread(&handle);
    *global_slot().lock() = None;
}

fn quit_hook_thread(handle: &HookHandle) {
    if handle.thread_id != 0 {
        unsafe {
            let _ = PostThreadMessageW(handle.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

fn run_hook_thread(tid_tx: std::sync::mpsc::Sender<u32>) {
    // Send our thread id so the controller can post WM_QUIT to us.
    let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    let _ = tid_tx.send(tid);

    let hook: HHOOK = match unsafe {
        SetWindowsHookExW(
            WH_MOUSE_LL,
            Some(hook_proc),
            HMODULE(std::ptr::null_mut()),
            0,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("SetWindowsHookExW(WH_MOUSE_LL) failed: {}", e);
            return;
        }
    };

    // Standard message pump. WH_MOUSE_LL events are dispatched to hook_proc
    // automatically by the OS; GetMessageW returning 0 means WM_QUIT was
    // posted, so we exit and unhook.
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // No TranslateMessage / DispatchMessage needed — this thread has
            // no UI, only the hook.
        }
        let _ = UnhookWindowsHookEx(hook);
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }
    let event = wparam.0 as u32;
    let is_button_down = matches!(
        event,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_NCLBUTTONDOWN
    );
    if !is_button_down {
        return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
    }
    // The lParam is a pointer to MSLLHOOKSTRUCT.
    let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    let cursor: POINT = info.pt;

    if let Some(app) = global_slot().lock().clone() {
        let inside = is_cursor_inside_popup(&app, cursor);
        if !inside {
            // Hop to the main thread to call window.hide(). Cannot do it
            // here — Tauri's window ops aren't safe to call from a hook
            // proc. This call is non-blocking.
            let app_for_main = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(w) = app_for_main.get_webview_window("tray-popup") {
                    let _ = w.hide();
                }
            });
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

fn is_cursor_inside_popup(app: &AppHandle, cursor: POINT) -> bool {
    let Some(window) = app.get_webview_window("tray-popup") else {
        return false;
    };
    let Ok(hwnd) = window.hwnd() else {
        return false;
    };
    let hwnd = windows::Win32::Foundation::HWND(hwnd.0 as _);
    let mut rect = RECT::default();
    let ok = unsafe { GetWindowRect(hwnd, &mut rect).is_ok() };
    if !ok {
        return false;
    }
    cursor.x >= rect.left
        && cursor.x <= rect.right
        && cursor.y >= rect.top
        && cursor.y <= rect.bottom
}
