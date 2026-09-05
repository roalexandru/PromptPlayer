//! Global shortcut registration.
//!
//! Defaults (all overridable from `promptplayer.yaml`, §7.2):
//! - **Arm/disarm**: ⌘⇧P (mac) / Ctrl+Shift+P (Windows) — toggles armed.
//! - **Command palette**: ⌥⌘\\ (mac) / Ctrl+Alt+\\ (Windows) — Spotlight-style picker.
//! - **Kill-switch**: ⌘⇧Esc (mac) / Ctrl+Alt+Shift+K (Windows) — abort playback.
//!   Windows reserves Ctrl+Shift+Esc for Task Manager, so we shift to a free combo.
//! - **Panic-reset**: ⌘⇧R (mac) / Ctrl+Alt+Shift+R (Windows) — release modifiers, force-disarm.
//! - **Next cue**: ⌘⇧. — fire the next setlist entry.
//! - **Pause/resume**: ⌘⇧, — freeze the running playback and pick it back up.
//! - **Faster / slower**: ⌥⌘⇧. and ⌥⌘⇧, — re-speed the running playback.
//!
//! Plus per-prompt hotkeys defined in `.pp.md` frontmatter, which are
//! re-registered on hot-reload via `rebuild_prompt_hotkeys`.

use crate::app::context::AppContext;
use crate::app::FireService;
use crate::config::AppConfig;
use crate::hotkey;
use crate::telemetry::{self, TelemetryEvent};
use std::str::FromStr;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Primary modifier — Cmd on macOS, Ctrl on Windows. Using `SUPER` directly
/// on Windows would map to the Win key, which collides with the OS-reserved
/// Win+Shift+P (projection menu) and other system globals.
#[cfg(target_os = "macos")]
const PRIMARY: Modifiers = Modifiers::SUPER;
#[cfg(not(target_os = "macos"))]
const PRIMARY: Modifiers = Modifiers::CONTROL;

/// Parse a user-supplied hotkey string, falling back to `default`.
///
/// A typo in `promptplayer.yaml` must never leave the app with no way to arm
/// or kill, so an unparseable string logs and yields the built-in chord
/// rather than dropping the binding.
fn resolve(configured: Option<&String>, default: Shortcut, label: &str) -> Shortcut {
    let Some(raw) = configured.map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return default;
    };
    let normalized = hotkey::normalize(raw);
    match Shortcut::from_str(&normalized) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                "hotkey-{} = {:?} is not a valid shortcut ({}); using the default",
                label,
                raw,
                e
            );
            default
        }
    }
}

/// The global chords in effect this session.
struct Globals {
    arm: Shortcut,
    picker: Shortcut,
    kill: Shortcut,
    panic: Shortcut,
    next_cue: Shortcut,
    pause: Shortcut,
    faster: Shortcut,
    slower: Shortcut,
}

fn resolve_globals(cfg: &AppConfig) -> Globals {
    let arm_default = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::KeyP);
    // ⌥⌘\ on mac, Ctrl+Alt+\ on Windows — free on stock OS, not used by major IDEs.
    let picker_default = Shortcut::new(Some(Modifiers::ALT | PRIMARY), Code::Backslash);
    // Kill: ⌘⇧Esc on mac. Windows reserves Ctrl+Shift+Esc for Task Manager,
    // so we use Ctrl+Alt+Shift+K instead.
    #[cfg(target_os = "macos")]
    let kill_default = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::Escape);
    #[cfg(not(target_os = "macos"))]
    let kill_default = Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::KeyK,
    );
    // Panic: ⌘⇧R on mac, Ctrl+Alt+Shift+R on Windows (avoids browser reload conflict).
    #[cfg(target_os = "macos")]
    let panic_default = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::KeyR);
    #[cfg(not(target_os = "macos"))]
    let panic_default = Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::KeyR,
    );
    // Transport controls. `.` / `,` with the primary modifier and Shift are
    // rare in editors, and the registration loop soft-fails on a conflict.
    let next_cue_default = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::Period);
    let pause_default = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::Comma);
    let faster_default = Shortcut::new(
        Some(Modifiers::ALT | Modifiers::SHIFT | PRIMARY),
        Code::Period,
    );
    let slower_default = Shortcut::new(
        Some(Modifiers::ALT | Modifiers::SHIFT | PRIMARY),
        Code::Comma,
    );

    Globals {
        arm: resolve(cfg.hotkey_arm.as_ref(), arm_default, "arm"),
        picker: resolve(cfg.hotkey_picker.as_ref(), picker_default, "picker"),
        kill: resolve(cfg.hotkey_kill.as_ref(), kill_default, "kill"),
        panic: resolve(cfg.hotkey_panic.as_ref(), panic_default, "panic"),
        next_cue: resolve(cfg.hotkey_next_cue.as_ref(), next_cue_default, "next-cue"),
        pause: resolve(cfg.hotkey_pause.as_ref(), pause_default, "pause"),
        faster: resolve(cfg.hotkey_faster.as_ref(), faster_default, "faster"),
        slower: resolve(cfg.hotkey_slower.as_ref(), slower_default, "slower"),
    }
}

pub fn register(
    app: &mut tauri::App,
    ctx: AppContext,
    fire: FireService,
) -> Result<(), Box<dyn std::error::Error>> {
    let g = resolve_globals(&ctx.config.get());
    let Globals {
        arm: shortcut_arm,
        picker: shortcut_picker,
        kill: shortcut_kill,
        panic: shortcut_panic,
        next_cue: shortcut_next_cue,
        pause: shortcut_pause,
        faster: shortcut_faster,
        slower: shortcut_slower,
    } = g;

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
                    refresh_tray_popup(&app_handle);
                } else if shortcut == &shortcut_next_cue {
                    // One key, next thing. Under stage pressure recall fails
                    // before dexterity does, so this is the gesture the picker
                    // can't replace.
                    match crate::commands::config::fire_next_cue_inner(
                        &app_handle,
                        &ctx_for_handler,
                    ) {
                        Ok(Some(id)) => tracing::info!("setlist cue fired → {}", id),
                        Ok(None) => tracing::info!("next-cue pressed but the setlist is empty"),
                        Err(e) => tracing::warn!("next-cue failed: {}", e),
                    }
                } else if shortcut == &shortcut_pause {
                    match ctx_for_handler.state.toggle_pause() {
                        Some(true) => tracing::info!("playback PAUSED"),
                        Some(false) => tracing::info!("playback RESUMED"),
                        None => tracing::debug!("pause pressed with nothing playing"),
                    }
                    refresh_tray_popup(&app_handle);
                } else if shortcut == &shortcut_faster || shortcut == &shortcut_slower {
                    let faster = shortcut == &shortcut_faster;
                    let factor = if faster {
                        crate::typer::SPEED_STEP
                    } else {
                        1.0 / crate::typer::SPEED_STEP
                    };
                    match ctx_for_handler.state.nudge_speed(factor) {
                        Some(s) => tracing::info!("playback speed → x{:.2}", s),
                        None => tracing::debug!("speed nudge with nothing playing"),
                    }
                    refresh_tray_popup(&app_handle);
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
    // Register globals with a soft-fail: on Windows, RegisterHotKey rejects
    // already-claimed combos (e.g. an installed app owns the same chord). Log
    // and continue so a single conflict doesn't kill the others.
    for (sc, label) in [
        (shortcut_arm, "arm/disarm"),
        (shortcut_picker, "command palette"),
        (shortcut_kill, "kill-switch"),
        (shortcut_panic, "panic-reset"),
        (shortcut_next_cue, "next cue"),
        (shortcut_pause, "pause/resume"),
        (shortcut_faster, "faster"),
        (shortcut_slower, "slower"),
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
    // Picker open path used by both global shortcut and tray menu. We hop to
    // the main thread because the AppKit calls (NSEvent.mouseLocation,
    // NSScreen.screens, setFrameOrigin:) require it. The actual sequence
    // (capture-if-not-visible, rebuild search, position, show) lives in
    // `commands::picker::summon_picker` so every entry point behaves alike.
    let app_for_main = app.clone();
    let ctx_for_main = ctx.clone();
    let _ = app.run_on_main_thread(move || {
        crate::commands::picker::summon_picker(&app_for_main, &ctx_for_main);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_or_absent_config_entry_uses_the_default() {
        let default = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::KeyP);
        assert_eq!(resolve(None, default, "arm"), default);
        assert_eq!(resolve(Some(&String::new()), default, "arm"), default);
        assert_eq!(resolve(Some(&"   ".to_string()), default, "arm"), default);
    }

    #[test]
    fn a_valid_config_entry_overrides_the_default() {
        let default = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::KeyP);
        let got = resolve(Some(&"ctrl+alt+j".to_string()), default, "arm");
        assert_ne!(got, default, "the configured chord must win");
    }

    #[test]
    fn an_unparseable_config_entry_falls_back_rather_than_dropping_the_binding() {
        // The file defines the kill switch; a typo must not disarm the app's
        // own safety mechanism.
        let default = Shortcut::new(Some(Modifiers::SHIFT | PRIMARY), Code::Escape);
        let got = resolve(Some(&"not a real chord".to_string()), default, "kill");
        assert_eq!(got, default);
    }

    #[test]
    fn defaults_are_all_distinct() {
        let g = resolve_globals(&AppConfig::default());
        let all = [
            g.arm, g.picker, g.kill, g.panic, g.next_cue, g.pause, g.faster, g.slower,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two default chords collide: {a:?}");
            }
        }
    }

    #[test]
    fn config_can_rebind_every_global() {
        let cfg = AppConfig {
            hotkey_arm: Some("ctrl+alt+1".into()),
            hotkey_picker: Some("ctrl+alt+2".into()),
            hotkey_kill: Some("ctrl+alt+3".into()),
            hotkey_panic: Some("ctrl+alt+4".into()),
            hotkey_next_cue: Some("ctrl+alt+5".into()),
            hotkey_pause: Some("ctrl+alt+6".into()),
            hotkey_faster: Some("ctrl+alt+7".into()),
            hotkey_slower: Some("ctrl+alt+8".into()),
            ..Default::default()
        };
        let g = resolve_globals(&cfg);
        let defaults = resolve_globals(&AppConfig::default());
        assert_ne!(g.arm, defaults.arm);
        assert_ne!(g.picker, defaults.picker);
        assert_ne!(g.kill, defaults.kill);
        assert_ne!(g.panic, defaults.panic);
        assert_ne!(g.next_cue, defaults.next_cue);
        assert_ne!(g.pause, defaults.pause);
        assert_ne!(g.faster, defaults.faster);
        assert_ne!(g.slower, defaults.slower);
    }
}
