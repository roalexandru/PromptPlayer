//! Tray-menu IPC commands.

use crate::app::context::AppContext;
use crate::commands::picker::show_picker_window;
use crate::error::{into_ipc, AppError, IpcResult};
use tauri::{AppHandle, Manager};

#[cfg(target_os = "macos")]
use crate::platform::macos as plat;
#[cfg(target_os = "windows")]
use crate::platform::windows as plat;

#[tauri::command]
#[specta::specta]
pub fn tray_open(
    app: AppHandle,
    target: String,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<()> {
    match target.as_str() {
        "library" => {
            show_window(&app, "library");
            Ok(())
        }
        "picker" => {
            ctx.focus.capture();
            ctx.search
                .lock()
                .rebuild_if_stale(ctx.prompts.generation(), &ctx.prompts.read());
            // Reposition the picker on whichever monitor the cursor is on
            // BEFORE showing. Without this, the picker stays on whatever
            // monitor it was last shown on — wrong when the user clicked
            // "Command palette" from the tray on a different monitor.
            // The global-shortcut path (`app::shortcuts::summon_picker`)
            // already does this; this matches that behavior.
            #[cfg(target_os = "macos")]
            crate::platform::macos::position_picker_on_cursor_screen(&app);
            #[cfg(target_os = "windows")]
            crate::platform::windows::position_picker_on_cursor_screen(&app);
            show_picker_window(&app);
            Ok(())
        }
        "about" => {
            // Custom branded window — replaces the system MessageDialog
            // (which renders the generic warning bubble icon and gives no
            // room for proper layout / version / "Check for updates"). The
            // window is registered as `about` in tauri.conf.json with fixed
            // size and no resizing.
            show_window(&app, "about");
            Ok(())
        }
        other => into_ipc(Err(AppError::InvalidArg(format!(
            "unknown tray target: {other}"
        )))),
    }
}

#[tauri::command]
#[specta::specta]
pub fn tray_quit(app: AppHandle) {
    app.exit(0);
}

/// Run a prompt from the tray (left-click on a pinned row, macOS only — the
/// Windows path goes through the native menu's `dispatch` instead). Hides the
/// popup first, then fires through the picker pipeline at human cadence. The
/// menu-bar / system-tray click never activates the app
/// (`NSNonactivatingPanelMask` on macOS, native menu on Windows), so the
/// original foreground app is still focused — no focus-restore dance needed.
#[tauri::command]
#[specta::specta]
pub fn tray_fire_prompt(
    app: AppHandle,
    prompt_id: String,
    ctx: tauri::State<'_, AppContext>,
) -> IpcResult<()> {
    if let Some(w) = app.get_webview_window("tray-popup") {
        let _ = w.hide();
    }
    #[cfg(target_os = "macos")]
    remove_outside_click_monitor_if_present(&app);
    let fire = crate::app::FireService::new(ctx.inner().clone(), app.clone());
    fire.fire_from_picker(&prompt_id, crate::app::fire::PickMode::Human);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn tray_popup_hide(app: AppHandle) -> IpcResult<()> {
    if let Some(w) = app.get_webview_window("tray-popup") {
        let _ = w.hide();
    }
    #[cfg(target_os = "macos")]
    remove_outside_click_monitor_if_present(&app);
    Ok(())
}

fn show_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        plat::activate_app();
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        // On macOS `.accessory` apps, focus transfer to a regular NSWindow
        // sometimes silently fails (no Dock icon for the OS to fall back on),
        // leaving the window buried behind whichever app the user clicked
        // from. `orderFrontRegardless` raises the z-order without requiring
        // activation — this is the AppKit-blessed "I really mean it" surface
        // path. No-op on Windows and on non-AppKit Mac contexts.
        #[cfg(target_os = "macos")]
        crate::platform::macos::order_window_front_regardless(&w);
    }
}

/// Show or hide the tray popup window in response to a click on the tray
/// icon. On Windows we delegate to a native `TrackPopupMenuEx` HMENU because
/// `WS_EX_NOACTIVATE` webview popups have no reliable outside-click dismiss
/// mechanism — the OS owns the entire interaction for native menus. On macOS
/// we use the existing NSPanel-based webview popup, which has working
/// outside-click dismiss via `NSEvent` global mouse-down monitor.
pub fn toggle_popup(app: &AppHandle, rect: tauri::Rect) {
    #[cfg(target_os = "windows")]
    {
        // `TrackPopupMenuEx` is modal: it blocks the calling thread (Tauri's
        // event loop) until the user picks an item or dismisses. There's no
        // "already visible" toggle state to track — Win32 owns it.
        crate::platform::windows::menu::show_tray_menu(app, rect);
        return;
    }

    #[cfg(target_os = "macos")]
    {
        let Some(window) = app.get_webview_window("tray-popup") else {
            return;
        };
        let already_visible = window.is_visible().unwrap_or(false);
        if already_visible {
            let _ = window.hide();
            remove_outside_click_monitor_if_present(app);
            return;
        }
        position_popup(&window, rect);
        use tauri::Emitter;
        let _ = window.emit("tray-popup-show", ());
        let _ = window.show();
        plat::order_panel_front_no_activate(&window);
        install_outside_click_monitor_if_needed(app);
    }
}

#[cfg(target_os = "macos")]
fn position_popup(window: &tauri::WebviewWindow, _rect: tauri::Rect) {
    // macOS: bypass Tauri's PhysicalPosition path entirely. On mixed-DPI
    // multi-monitor setups, monitor physical-pixel bounds can OVERLAP (a
    // retina laptop at scale=2 has its right edge at 3024px while a 1x
    // external at logical x=1512 starts at physical x=1512). That makes
    // "find the monitor whose physical bounds contain icon_phys" ambiguous,
    // and `set_position(PhysicalPosition)` lands on the wrong monitor.
    //
    // AppKit's NSEvent.mouseLocation + NSScreen.screens use logical points
    // in a single unified coord space across all monitors regardless of
    // DPI. setFrameOrigin: (also AppKit logical pt) places the window
    // unambiguously. This is the same recipe the picker palette uses.
    crate::platform::macos::position_popover_under_cursor(window);
}

#[cfg(target_os = "macos")]
fn install_outside_click_monitor_if_needed(_app: &AppHandle) {
    use std::sync::Arc;
    if let Some(monitor) = _app.try_state::<Arc<crate::platform::macos::OutsideClickMonitor>>() {
        crate::platform::macos::install_outside_click_monitor(_app, monitor.inner());
    }
}

#[cfg(target_os = "macos")]
fn remove_outside_click_monitor_if_present(_app: &AppHandle) {
    use std::sync::Arc;
    if let Some(monitor) = _app.try_state::<Arc<crate::platform::macos::OutsideClickMonitor>>() {
        crate::platform::macos::remove_outside_click_monitor(monitor.inner());
    }
}
