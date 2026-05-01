//! Global shortcut registration.
//!
//! Three permanent shortcuts:
//! - **Arm/disarm**: ⌘⇧P — toggles the global "armed" flag.
//! - **Command palette**: ⌥⌘\\ — Spotlight-style picker.
//! - **Kill-switch**: ⌘⇧Esc — abort current playback (§2.7).
//! - **Panic-reset**: ⌘⇧R — release modifiers, force-disarm, hide picker.
//!
//! Plus per-prompt hotkeys defined in `.pp.md` frontmatter, which are
//! re-registered on hot-reload via `rebuild_prompt_hotkeys`.

use crate::app::context::AppContext;
use crate::app::FireService;
use crate::hotkey;
use crate::telemetry::{self, TelemetryEvent};
use std::str::FromStr;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

pub fn register(
    app: &mut tauri::App,
    ctx: AppContext,
    fire: FireService,
) -> Result<(), Box<dyn std::error::Error>> {
    let shortcut_arm = Shortcut::new(Some(Modifiers::SHIFT | Modifiers::SUPER), Code::KeyP);
    // ⌥⌘\ — free on stock macOS; not used by Finder, Safari, or major IDEs.
    let shortcut_picker = Shortcut::new(
        Some(Modifiers::ALT | Modifiers::SUPER),
        Code::Backslash,
    );
    let shortcut_kill = Shortcut::new(Some(Modifiers::SHIFT | Modifiers::SUPER), Code::Escape);
    let shortcut_panic = Shortcut::new(Some(Modifiers::SHIFT | Modifiers::SUPER), Code::KeyR);

    let app_handle = app.handle().clone();
    let ctx_for_handler = ctx.clone();
    let fire_for_handler = fire.clone();

    app.handle().plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_handler(move |_app, shortcut, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                if shortcut == &shortcut_arm {
                    let new = ctx_for_handler.state.toggle_armed();
                    tracing::info!("hotkey arm → enabled={}", new);
                    refresh_tray_popup(&app_handle);
                    telemetry::send(&app_handle, TelemetryEvent::ArmToggled { armed: new });
                } else if shortcut == &shortcut_picker {
                    summon_picker(&app_handle, &ctx_for_handler);
                } else if shortcut == &shortcut_kill {
                    tracing::warn!("KILL-SWITCH invoked");
                    ctx_for_handler.state.cancel_playback();
                    telemetry::send(&app_handle, TelemetryEvent::PromptKilled);
                } else if shortcut == &shortcut_panic {
                    tracing::warn!("PANIC-RESET invoked");
                    ctx_for_handler.state.cancel_playback();
                    ctx_for_handler.state.set_armed(false);
                    if let Ok(mut inj) = crate::inject::EnigoInjector::new() {
                        use crate::typer::Injector;
                        inj.release_all_modifiers();
                    }
                    if let Some(w) = app_handle.get_webview_window("picker") {
                        let _ = w.hide();
                    }
                } else {
                    // Per-prompt hotkey lookup.
                    let map = ctx_for_handler.hotkeys.read();
                    for (hk_str, prompt_id) in map.iter() {
                        if let Ok(s) = Shortcut::from_str(&hotkey::normalize(hk_str)) {
                            if &s == shortcut {
                                tracing::info!("prompt hotkey {} fired → {}", hk_str, prompt_id);
                                fire_for_handler.fire_from_hotkey(prompt_id);
                                break;
                            }
                        }
                    }
                }
            })
            .build(),
    )?;

    let gs = app.global_shortcut();
    gs.register(shortcut_arm)?;
    gs.register(shortcut_picker)?;
    gs.register(shortcut_kill)?;
    gs.register(shortcut_panic)?;
    Ok(())
}

fn summon_picker(app: &AppHandle, ctx: &AppContext) {
    // Picker open path used by both global shortcut and tray menu. We hop to
    // the main thread because the AppKit calls (NSEvent.mouseLocation,
    // NSScreen.screens, setFrameOrigin:) require it.
    let app_for_main = app.clone();
    let ctx_for_main = ctx.clone();
    let _ = app.run_on_main_thread(move || {
        ctx_for_main.focus.capture();
        // Rebuild search index lazily — only if our generation lags the
        // PromptStore generation.
        ctx_for_main.search.lock().rebuild_if_stale(
            ctx_for_main.prompts.generation(),
            &ctx_for_main.prompts.read(),
        );
        #[cfg(target_os = "macos")]
        crate::platform::macos::position_picker_on_cursor_screen(&app_for_main);
        crate::commands::picker::show_picker_window(&app_for_main);
        telemetry::send(&app_for_main, TelemetryEvent::PickerOpened);
    });
}

/// Re-register all prompt hotkeys after a library hot-reload or save.
pub fn rebuild_prompt_hotkeys(app: &AppHandle, ctx: &AppContext) {
    let prompts = ctx.prompts.snapshot();
    let gs = app.global_shortcut();
    {
        let old = ctx.hotkeys.read();
        for hk_str in old.keys() {
            if let Ok(s) = Shortcut::from_str(&hotkey::normalize(hk_str)) {
                let _ = gs.unregister(s);
            }
        }
    }
    let mut new_map = std::collections::HashMap::new();
    for p in &prompts {
        if !p.enabled {
            continue;
        }
        let Some(hk) = &p.hotkey else { continue };
        if hk.trim().is_empty() {
            continue;
        }
        let normalized = hotkey::normalize(hk);
        match Shortcut::from_str(&normalized) {
            Ok(s) => match gs.register(s) {
                Ok(()) => {
                    tracing::info!("registered hotkey {} → {}", hk, p.id);
                    new_map.insert(hk.clone(), p.id.clone());
                }
                Err(e) => tracing::warn!("hotkey {} register failed: {}", hk, e),
            },
            Err(e) => tracing::warn!(
                "hotkey {} unparseable (normalized: {}): {}",
                hk,
                normalized,
                e
            ),
        }
    }
    *ctx.hotkeys.write() = new_map;
}

pub fn refresh_tray_popup(app: &AppHandle) {
    use tauri::Emitter;
    if let Some(window) = app.get_webview_window("tray-popup") {
        let _ = window.emit("tray-popup-show", ());
    }
}

