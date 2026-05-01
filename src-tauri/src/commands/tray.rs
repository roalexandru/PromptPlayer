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
            show_picker_window(&app);
            Ok(())
        }
        "settings" => {
            show_window(&app, "settings");
            Ok(())
        }
        "about" => {
            use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
            app.dialog()
                .message(format!(
                    "Prompt Player v{}\n\nStealth keyboard utility for live demos.\nBundle ID: com.roalexandru.promptplayer",
                    env!("CARGO_PKG_VERSION")
                ))
                .kind(MessageDialogKind::Info)
                .title("About Prompt Player")
                .show(|_| {});
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
    let scale = window.scale_factor().unwrap_or(1.0);
    let icon_pos = rect.position.to_physical::<f64>(scale);
    #[cfg(target_os = "windows")]
    let icon_size = rect.size.to_physical::<f64>(scale);
    let outer = window.outer_size().ok();
    let Some(size) = outer else { return };
    let win_w = size.width as f64;
    let win_h = size.height as f64;

    // Default placement (anchor below icon, left edge aligned — matches
    // native NSMenu placement on Mac). On Windows this is overridden per
    // taskbar edge in `position_for_taskbar_edge`.
    #[cfg(not(target_os = "windows"))]
    let (mut x, mut y) = (icon_pos.x, icon_pos.y + 4.0);

    // SHAppBarMessage tells us where the taskbar lives so we can anchor the
    // popup on the side OPPOSITE the taskbar edge — same idiom Win11's
    // Quick Settings popover uses.
    #[cfg(target_os = "windows")]
    let (mut x, mut y) = {
        let edge = crate::platform::windows::taskbar_edge();
        position_for_taskbar_edge(edge, icon_pos, icon_size, win_w, win_h)
    };

    // Clamp so the window stays on the same screen the icon is on (the
    // tray-icon rect can sit at a screen edge).
    if let Some(monitor) = window.current_monitor().ok().flatten() {
        let m_pos = monitor.position();
        let m_size = monitor.size();
        let right_edge = (m_pos.x as f64) + (m_size.width as f64) - 4.0;
        let bottom_edge = (m_pos.y as f64) + (m_size.height as f64) - 4.0;
        if x + win_w > right_edge {
            x = right_edge - win_w;
        }
        if x < (m_pos.x as f64) + 4.0 {
            x = (m_pos.x as f64) + 4.0;
        }
        if y + win_h > bottom_edge {
            y = bottom_edge - win_h;
        }
        if y < (m_pos.y as f64) + 4.0 {
            y = (m_pos.y as f64) + 4.0;
        }
    }
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
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
