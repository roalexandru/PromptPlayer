//! Tray-menu IPC commands.

use crate::app::context::AppContext;
use crate::commands::picker::show_picker_window;
use crate::error::{into_ipc, AppError, IpcResult};
use tauri::{AppHandle, Manager};

#[tauri::command]
#[specta::specta]
pub fn tray_open(app: AppHandle, target: String, ctx: tauri::State<'_, AppContext>) -> IpcResult<()> {
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
        other => into_ipc(Err(AppError::InvalidArg(format!("unknown tray target: {other}")))),
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
    #[cfg(target_os = "macos")]
    {
        use std::sync::Arc;
        if let Some(monitor) =
            app.try_state::<Arc<crate::platform::macos::OutsideClickMonitor>>()
        {
            crate::platform::macos::remove_outside_click_monitor(monitor.inner());
        }
    }
    Ok(())
}

fn show_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        #[cfg(target_os = "macos")]
        crate::platform::macos::activate_app();
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Show or hide the tray popup window in response to a left-click on the
/// tray icon. Position the window immediately under the tray icon's rect so
/// it visually anchors to the menu bar (matches native NSMenu placement).
pub fn toggle_popup(app: &AppHandle, rect: tauri::Rect) {
    let Some(window) = app.get_webview_window("tray-popup") else {
        return;
    };
    let already_visible = window.is_visible().unwrap_or(false);
    if already_visible {
        let _ = window.hide();
        #[cfg(target_os = "macos")]
        {
            use std::sync::Arc;
            if let Some(monitor) =
                app.try_state::<Arc<crate::platform::macos::OutsideClickMonitor>>()
            {
                crate::platform::macos::remove_outside_click_monitor(monitor.inner());
            }
        }
        return;
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    let icon_pos = rect.position.to_physical::<f64>(scale);
    let outer = window.outer_size().ok();
    if let Some(size) = outer {
        let win_w = size.width as f64;
        let mut x = icon_pos.x;
        if let Some(monitor) = window.current_monitor().ok().flatten() {
            let m_pos = monitor.position();
            let m_size = monitor.size();
            let right_edge = (m_pos.x as f64) + (m_size.width as f64) - 4.0;
            if x + win_w > right_edge {
                x = right_edge - win_w;
            }
            if x < (m_pos.x as f64) + 4.0 {
                x = (m_pos.x as f64) + 4.0;
            }
        }
        let y = icon_pos.y + 4.0;
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
    use tauri::Emitter;
    let _ = window.emit("tray-popup-show", ());
    let _ = window.show();
    #[cfg(target_os = "macos")]
    crate::platform::macos::order_panel_front_no_activate(&window);
    #[cfg(target_os = "macos")]
    {
        use std::sync::Arc;
        if let Some(monitor) =
            app.try_state::<Arc<crate::platform::macos::OutsideClickMonitor>>()
        {
            crate::platform::macos::install_outside_click_monitor(app, monitor.inner());
        }
    }
}
