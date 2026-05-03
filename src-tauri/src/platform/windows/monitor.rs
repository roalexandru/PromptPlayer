//! Windows outside-click monitor — polls `GetForegroundWindow` to dismiss
//! the tray popup when the user clicks anywhere outside it.
//!
//! Why polling, not a low-level hook: we previously used `WH_MOUSE_LL`
//! (system-wide mouse hook) but it silently fails for some users — Windows
//! 11 has tightened low-level hook restrictions and EDR/AV products
//! frequently block them. The popup would then linger forever because
//! `WS_EX_NOACTIVATE` also kills `WindowEvent::Focused(false)`. Polling
//! `GetForegroundWindow` every 150 ms is dead simple, has no permission
//! requirements, and works under any sandboxing posture.
//!
//! How dismiss is detected: when we install, we record the popup's HWND.
//! Each tick we read `GetForegroundWindow()`; if it's any non-null window
//! that's not our popup (the popup never becomes foreground anyway because
//! of `WS_EX_NOACTIVATE`), then the user has clicked or alt-tabbed
//! elsewhere and we hide the popup. Tauri window ops aren't safe off the
//! main thread, so the actual `hide()` is dispatched via
//! `AppHandle::run_on_main_thread`.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// Anti-bounce window: if the poller hides the popup within this many
/// millis of a tray-icon click, the click was the dismissal — not a
/// re-open. `toggle_popup` checks `recently_dismissed()` before re-showing.
const DISMISS_DEBOUNCE_MS: i64 = 250;
/// Poller cadence — 150 ms is fast enough to feel instantaneous on a
/// click-anywhere-to-dismiss interaction without burning measurable CPU.
const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Per-process state: holds the running poller thread (if any) and the
/// last-hidden timestamp used by the tray-icon-click anti-bounce.
pub struct OutsideClickMonitor {
    inner: Mutex<Option<PollerHandle>>,
    last_hidden_ms: AtomicI64,
}

struct PollerHandle {
    stop: Arc<AtomicBool>,
    _join: Option<JoinHandle<()>>,
}

impl OutsideClickMonitor {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(None),
            last_hidden_ms: AtomicI64::new(0),
        })
    }

    fn mark_hidden_now(&self) {
        self.last_hidden_ms.store(now_millis(), Ordering::Release);
    }

    /// True if the poller hid the popup within `DISMISS_DEBOUNCE_MS`.
    /// Callers (tray-icon click handler) should treat this as "user just
    /// dismissed — don't re-open."
    pub fn recently_dismissed(&self) -> bool {
        let last = self.last_hidden_ms.load(Ordering::Acquire);
        if last == 0 {
            return false;
        }
        now_millis() - last < DISMISS_DEBOUNCE_MS
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Drop for OutsideClickMonitor {
    fn drop(&mut self) {
        if let Some(handle) = self.inner.lock().take() {
            handle.stop.store(true, Ordering::Release);
        }
    }
}

pub fn install_outside_click_monitor(app: &AppHandle, monitor: &Arc<OutsideClickMonitor>) {
    if monitor.inner.lock().is_some() {
        return; // already installed
    }
    // Capture the popup's HWND right at install time. The popup is the only
    // window we care about; everything else is "outside."
    let popup_hwnd_raw = app
        .get_webview_window("tray-popup")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as isize);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = stop.clone();
    let app_for_thread = app.clone();
    let monitor_for_thread = monitor.clone();

    let join = thread::Builder::new()
        .name("promptplayer-fg-poll".into())
        .spawn(move || {
            run_poller(
                stop_for_thread,
                app_for_thread,
                monitor_for_thread,
                popup_hwnd_raw,
            )
        })
        .expect("spawn foreground-poller thread");

    *monitor.inner.lock() = Some(PollerHandle {
        stop,
        _join: Some(join),
    });
}

pub fn remove_outside_click_monitor(monitor: &Arc<OutsideClickMonitor>) {
    let Some(handle) = monitor.inner.lock().take() else {
        return;
    };
    handle.stop.store(true, Ordering::Release);
    // We don't join — the thread will notice the flag on its next tick
    // (within POLL_INTERVAL) and exit cleanly. Joining here would block
    // the caller for up to 150 ms, which is enough to feel sluggish in
    // the tray-click dismiss path.
}

fn run_poller(
    stop: Arc<AtomicBool>,
    app: AppHandle,
    monitor: Arc<OutsideClickMonitor>,
    popup_hwnd_raw: Option<isize>,
) {
    // Brief grace period so the popup is fully on-screen before we start
    // polling — if we ticked instantly, GetForegroundWindow could still be
    // pointing at the user's previous app and we'd hide on the very first
    // tick (the popup has WS_EX_NOACTIVATE so it never *becomes* foreground).
    thread::sleep(Duration::from_millis(120));

    while !stop.load(Ordering::Acquire) {
        let fg = unsafe { GetForegroundWindow() };
        if !fg.is_invalid() {
            // The popup is non-activating, so foreground will never be its
            // HWND — but check anyway in case Windows briefly assigns it
            // during a system transition.
            let fg_raw = fg.0 as isize;
            let is_popup = matches!(popup_hwnd_raw, Some(h) if h == fg_raw);
            if !is_popup {
                // User clicked / focused something other than us. Hide.
                monitor.mark_hidden_now();
                let app_for_main = app.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Some(w) = app_for_main.get_webview_window("tray-popup") {
                        let _ = w.hide();
                    }
                });
                return;
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}
