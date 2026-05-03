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

/// Run a prompt from the tray (left-click on a pinned row). Hides the popup
/// first, then fires through the picker pipeline at human cadence. The
/// menu-bar / system-tray click never activates the app (`NSNonactivatingPanelMask`
/// on macOS, `WS_EX_NOACTIVATE` on Windows), so the original foreground app
/// is still focused — no focus-restore dance needed.
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
    }
}

/// Show or hide the tray popup window in response to a left-click on the
/// tray icon. Position the window immediately under the tray icon's rect so
/// it visually anchors to the menu bar (matches native NSMenu placement on
/// Mac; on Windows we additionally branch on taskbar edge — see
/// `position_for_taskbar_edge`).
pub fn toggle_popup(app: &AppHandle, rect: tauri::Rect) {
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
    // On Mac, show first so makeKeyAndOrderFront: has a visible target. On
    // Windows, we don't call show() here — it triggers ShowWindow(SW_SHOW)
    // which would activate the app despite WS_EX_NOACTIVATE in some
    // configurations. order_panel_front_no_activate uses SW_SHOWNOACTIVATE
    // explicitly.
    #[cfg(target_os = "macos")]
    let _ = window.show();
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    plat::order_panel_front_no_activate(&window);
    install_outside_click_monitor_if_needed(app);
}

fn position_popup(window: &tauri::WebviewWindow, rect: tauri::Rect) {
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
    #[cfg(target_os = "macos")]
    {
        let _ = rect; // size/position not needed; we use the live cursor.
        crate::platform::macos::position_popover_under_cursor(window);
        return;
    }

    // Windows-only path below. The Tauri PhysicalPosition coord space is
    // unambiguous on Win32 (no per-monitor pixel overlaps).
    #[cfg(target_os = "windows")]
    {
        // Tauri normalizes tray rects to PhysicalPosition on Windows. We work
        // entirely in physical pixels for placement.
        let icon_phys = match rect.position {
            tauri::Position::Physical(p) => tauri::PhysicalPosition::new(p.x as f64, p.y as f64),
            tauri::Position::Logical(_) => {
                // Tauri 2 uses Physical for tray rects; convert via window scale
                // as a defensive fallback if that ever changes.
                let scale = window.scale_factor().unwrap_or(1.0);
                rect.position.to_physical::<f64>(scale)
            }
        };
        #[cfg(target_os = "windows")]
        let icon_size_phys = match rect.size {
            tauri::Size::Physical(s) => tauri::PhysicalSize::new(s.width as f64, s.height as f64),
            tauri::Size::Logical(_) => {
                let scale = window.scale_factor().unwrap_or(1.0);
                rect.size.to_physical::<f64>(scale)
            }
        };

        // Find the monitor that contains the tray icon's CENTER point. Walking
        // available_monitors() is the only Tauri-2 API that doesn't presuppose
        // which monitor is "current".
        let target_monitor = window
            .available_monitors()
            .ok()
            .and_then(|monitors| {
                monitors.into_iter().find(|m| {
                    let mp = m.position();
                    let ms = m.size();
                    let mx = mp.x as f64;
                    let my = mp.y as f64;
                    let mw = ms.width as f64;
                    let mh = ms.height as f64;
                    icon_phys.x >= mx
                        && icon_phys.x < mx + mw
                        && icon_phys.y >= my
                        && icon_phys.y < my + mh
                })
            })
            // Fallback: if no monitor matched (unlikely, but possible if the
            // icon is at a screen-edge boundary), use whatever monitor the
            // popover currently lives on.
            .or_else(|| window.current_monitor().ok().flatten());

        let outer = window.outer_size().ok();
        let Some(size) = outer else { return };
        let win_w = size.width as f64;
        let win_h = size.height as f64;

        // Default placement: anchor BELOW the icon, left edge aligned
        // (native NSMenu placement on Mac).
        #[cfg(not(target_os = "windows"))]
        let (mut x, mut y) = (icon_phys.x, icon_phys.y + 4.0);

        // Windows: pick anchor side based on taskbar edge.
        #[cfg(target_os = "windows")]
        let (mut x, mut y) = {
            let edge = crate::platform::windows::taskbar_edge();
            position_for_taskbar_edge(edge, icon_phys, icon_size_phys, win_w, win_h)
        };

        // Clamp to the TARGET monitor's bounds (not the popover's previous
        // monitor). This is what was making the popup reappear on monitor 1
        // after a click on monitor 2.
        if let Some(monitor) = target_monitor {
            let mp = monitor.position();
            let ms = monitor.size();
            let m_left = mp.x as f64 + 4.0;
            let m_top = mp.y as f64 + 4.0;
            let m_right = (mp.x as f64) + (ms.width as f64) - 4.0;
            let m_bottom = (mp.y as f64) + (ms.height as f64) - 4.0;
            if x + win_w > m_right {
                x = m_right - win_w;
            }
            if x < m_left {
                x = m_left;
            }
            if y + win_h > m_bottom {
                y = m_bottom - win_h;
            }
            if y < m_top {
                y = m_top;
            }
        }
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    } // end #[cfg(target_os = "windows")] block
}

#[cfg(target_os = "windows")]
fn position_for_taskbar_edge(
    edge: crate::platform::windows::TaskbarEdge,
    icon_pos: tauri::PhysicalPosition<f64>,
    icon_size: tauri::PhysicalSize<f64>,
    win_w: f64,
    win_h: f64,
) -> (f64, f64) {
    use crate::platform::windows::TaskbarEdge::*;
    match edge {
        // Default Win10/11 layout — taskbar at bottom, popup grows upward.
        Bottom => (icon_pos.x, icon_pos.y - win_h - 4.0),
        Top => (icon_pos.x, icon_pos.y + icon_size.height + 4.0),
        Left => (icon_pos.x + icon_size.width + 4.0, icon_pos.y),
        Right => (icon_pos.x - win_w - 4.0, icon_pos.y),
    }
}

fn install_outside_click_monitor_if_needed(_app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        use std::sync::Arc;
        if let Some(monitor) = _app.try_state::<Arc<crate::platform::macos::OutsideClickMonitor>>()
        {
            crate::platform::macos::install_outside_click_monitor(_app, monitor.inner());
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::sync::Arc;
        if let Some(monitor) =
            _app.try_state::<Arc<crate::platform::windows::OutsideClickMonitor>>()
        {
            crate::platform::windows::install_outside_click_monitor(_app, monitor.inner());
        }
    }
}

fn remove_outside_click_monitor_if_present(_app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        use std::sync::Arc;
        if let Some(monitor) = _app.try_state::<Arc<crate::platform::macos::OutsideClickMonitor>>()
        {
            crate::platform::macos::remove_outside_click_monitor(monitor.inner());
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::sync::Arc;
        if let Some(monitor) =
            _app.try_state::<Arc<crate::platform::windows::OutsideClickMonitor>>()
        {
            crate::platform::windows::remove_outside_click_monitor(monitor.inner());
        }
    }
}
