//! Global shortcuts: arm/disarm, command palette, kill-switch and panic-reset,
//! each defined below (Windows shifts the kill combo off the OS-reserved
//! Ctrl+Shift+Esc). Per-prompt hotkeys from `.pp.md` frontmatter re-register on
//! hot-reload via `rebuild_prompt_hotkeys`.

use crate::app::context::AppContext;
use crate::app::FireService;
use crate::hotkey;
use crate::telemetry::{self, CancelReason, HotkeyFailReason, PickerSource, TelemetryEvent};
use std::str::FromStr;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Cmd on macOS, Ctrl on Windows. `SUPER` would be the Win key there, which
/// collides with OS-reserved combos like Win+Shift+P.
#[cfg(target_os = "macos")]
const PRIMARY: Modifiers = Modifiers::SUPER;
#[cfg(not(target_os = "macos"))]
const PRIMARY: Modifiers = Modifiers::CONTROL;

pub fn register(
    app: &mut tauri::App,
    ctx: AppContext,
    fire: FireService,
) -> Result<(), Box<dyn std::error::Error>> {
    let shortcut_arm = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::KeyP);
    // ⌥⌘\ on mac, Ctrl+Alt+\ on Windows — free on stock OS, not used by major IDEs.
    let shortcut_picker = Shortcut::new(Some(Modifiers::ALT | PRIMARY), Code::Backslash);
    // Kill: ⌘⇧Esc on mac. Windows reserves Ctrl+Shift+Esc for Task Manager,
    // so we use Ctrl+Alt+Shift+K instead.
    #[cfg(target_os = "macos")]
    let shortcut_kill = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::Escape);
    #[cfg(not(target_os = "macos"))]
    let shortcut_kill = Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::KeyK,
    );
    // Panic: ⌘⇧R on mac, Ctrl+Alt+Shift+R on Windows (avoids browser reload conflict).
    #[cfg(target_os = "macos")]
    let shortcut_panic = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::KeyR);
    #[cfg(not(target_os = "macos"))]
    let shortcut_panic = Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::KeyR,
    );

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
                    set_armed_and_report(&app_handle, &ctx_for_handler, new);
                } else if shortcut == &shortcut_picker {
                    summon_picker(&app_handle, &ctx_for_handler);
                } else if shortcut == &shortcut_kill {
                    tracing::warn!("KILL-SWITCH invoked");
                    let was_playing = ctx_for_handler.state.is_playing();
                    ctx_for_handler
                        .state
                        .cancel_playback_with(CancelReason::Kill);
                    telemetry::send(&app_handle, TelemetryEvent::PromptKilled { was_playing });
                } else if shortcut == &shortcut_panic {
                    tracing::warn!("PANIC-RESET invoked");
                    let was_playing = ctx_for_handler.state.is_playing();
                    ctx_for_handler
                        .state
                        .cancel_playback_with(CancelReason::Kill);
                    telemetry::send(&app_handle, TelemetryEvent::PromptKilled { was_playing });
                    if ctx_for_handler.state.is_armed() {
                        set_armed_and_report(&app_handle, &ctx_for_handler, false);
                    }
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
    // Soft-fail: Windows rejects combos another app already claimed, and one
    // conflict must not take out the rest.
    for (sc, label) in [
        (shortcut_arm, "arm/disarm"),
        (shortcut_picker, "command palette"),
        (shortcut_kill, "kill-switch"),
        (shortcut_panic, "panic-reset"),
    ] {
        if let Err(e) = gs.register(sc) {
            tracing::warn!(
                "global shortcut '{}' failed to register: {} — likely claimed by another app",
                label,
                e
            );
        }
    }
    Ok(())
}

fn summon_picker(app: &AppHandle, ctx: &AppContext) {
    // Hop to the main thread: the AppKit positioning calls require it. The
    // sequence itself lives in `commands::picker::summon_picker`.
    let app_for_main = app.clone();
    let ctx_for_main = ctx.clone();
    let _ = app.run_on_main_thread(move || {
        crate::commands::picker::summon_picker(
            &app_for_main,
            &ctx_for_main,
            PickerSource::Shortcut,
            crate::commands::picker::FocusCapture::Take,
        );
    });
}

/// Apply an armed state everywhere: runtime flag, persisted setting, tray, and
/// telemetry. `hook_alive` rides along because arming a dead hook does nothing.
pub fn set_armed_and_report(app: &AppHandle, ctx: &AppContext, armed: bool) {
    ctx.state.set_armed(armed);
    ctx.settings.update(|s| s.armed = armed);
    refresh_tray_popup(app);
    telemetry::send(
        app,
        TelemetryEvent::ArmToggled {
            armed,
            hook_alive: ctx.state.hook_alive(),
        },
    );
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
        // A hotkey that fails to register just silently never works, so both
        // failure modes are reported rather than only logged.
        match Shortcut::from_str(&normalized) {
            Ok(s) => match gs.register(s) {
                Ok(()) => {
                    tracing::info!("registered hotkey {} → {}", hk, p.id);
                    new_map.insert(hk.clone(), p.id.clone());
                }
                Err(e) => {
                    tracing::warn!("hotkey {} register failed: {}", hk, e);
                    telemetry::send(
                        app,
                        TelemetryEvent::HotkeyRegisterFailed {
                            reason: HotkeyFailReason::Conflict,
                        },
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    "hotkey {} unparseable (normalized: {}): {}",
                    hk,
                    normalized,
                    e
                );
                telemetry::send(
                    app,
                    TelemetryEvent::HotkeyRegisterFailed {
                        reason: HotkeyFailReason::Unparseable,
                    },
                );
            }
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
