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

/// The global chords in effect right now.
///
/// Held behind an `RwLock` on `AppContext` rather than captured by the handler
/// closure, so a rebind from `promptplayer.yaml` can take effect without a
/// relaunch: `reregister_globals` swaps this and re-registers with the OS.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Globals {
    arm: Shortcut,
    picker: Shortcut,
    kill: Shortcut,
    panic: Shortcut,
    next_cue: Shortcut,
    pause: Shortcut,
    faster: Shortcut,
    slower: Shortcut,
}

impl Globals {
    /// Every chord, paired with a label for the soft-fail registration log.
    fn labelled(&self) -> [(Shortcut, &'static str); 8] {
        [
            (self.arm, "arm/disarm"),
            (self.picker, "command palette"),
            (self.kill, "kill-switch"),
            (self.panic, "panic-reset"),
            (self.next_cue, "next cue"),
            (self.pause, "pause/resume"),
            (self.faster, "faster"),
            (self.slower, "slower"),
        ]
    }
}

impl Default for Globals {
    fn default() -> Self {
        resolve_globals(&AppConfig::default())
    }
}

/// Shared handle to the chords currently registered.
pub type GlobalHotkeys = std::sync::Arc<parking_lot::RwLock<Globals>>;

pub fn resolve_globals(cfg: &AppConfig) -> Globals {
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
    // Adopt whatever the config resolved to, then let the handler read the
    // shared copy on every keypress so a later rebind needs no restart.
    *ctx.globals.write() = resolve_globals(&ctx.config.get());

    let app_handle = app.handle().clone();
    let ctx_for_handler = ctx.clone();
    let fire_for_handler = fire.clone();

    app.handle().plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_handler(move |_app, shortcut, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                // Snapshot the current bindings; `Globals` is `Copy`, so this
                // is a cheap read that can't hold the lock across a fire.
                let g = *ctx_for_handler.globals.read();
                let shortcut_arm = &g.arm;
                let shortcut_picker = &g.picker;
                let shortcut_kill = &g.kill;
                let shortcut_panic = &g.panic;
                let shortcut_next_cue = &g.next_cue;
                let shortcut_pause = &g.pause;
                let shortcut_faster = &g.faster;
                let shortcut_slower = &g.slower;
                if shortcut == shortcut_arm {
                    let new = ctx_for_handler.state.toggle_armed();
                    tracing::info!("hotkey arm → enabled={}", new);
                    set_armed_and_report(&app_handle, &ctx_for_handler, new);
                } else if shortcut == shortcut_picker {
                    summon_picker(&app_handle, &ctx_for_handler);
                } else if shortcut == shortcut_kill {
                    tracing::warn!("KILL-SWITCH invoked");
                    let was_playing = ctx_for_handler.state.is_playing();
                    ctx_for_handler
                        .state
                        .cancel_playback_with(CancelReason::Kill);
                    // §2.7 — the only feedback that an abort landed, since
                    // cancellation is otherwise silent by design.
                    crate::app::tray_flash::flash_kill(&app_handle);
                    telemetry::send(&app_handle, TelemetryEvent::PromptKilled { was_playing });
                } else if shortcut == shortcut_panic {
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
                    crate::app::tray_flash::flash_kill(&app_handle);
                    refresh_tray_popup(&app_handle);
                } else if shortcut == shortcut_next_cue {
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
                } else if shortcut == shortcut_pause {
                    match ctx_for_handler.state.toggle_pause() {
                        Some(true) => tracing::info!("playback PAUSED"),
                        Some(false) => tracing::info!("playback RESUMED"),
                        None => tracing::debug!("pause pressed with nothing playing"),
                    }
                    refresh_tray_popup(&app_handle);
                } else if shortcut == shortcut_faster || shortcut == shortcut_slower {
                    let faster = shortcut == shortcut_faster;
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

    register_current(app.handle(), &ctx);
    Ok(())
}

/// Register the chords in `ctx.globals` with the OS.
///
/// Soft-fails per chord: on Windows `RegisterHotKey` rejects an
/// already-claimed combo (an installed app owns the same chord), and a single
/// conflict must not cost us the other seven. Returns the human-readable
/// failures so a rebind can report them.
fn register_current(app: &AppHandle, ctx: &AppContext) -> Vec<String> {
    let gs = app.global_shortcut();
    // Soft-fail: Windows rejects combos another app already claimed, and one
    // conflict must not take out the rest.
    let mut failures = Vec::new();
    for (sc, label) in ctx.globals.read().labelled() {
        if let Err(e) = gs.register(sc) {
            tracing::warn!(
                "global shortcut '{}' failed to register: {} — likely claimed by another app",
                label,
                e
            );
            failures.push(format!("{label} is already claimed by another app"));
        }
    }
    failures
}

/// Re-resolve the global chords from the current config and swap them live.
///
/// Without this a rebind in `promptplayer.yaml` needed a relaunch, because the
/// chords were baked into the handler closure at startup. Returns the chords
/// the OS refused (see `register_current`).
pub fn reregister_globals(app: &AppHandle, ctx: &AppContext) -> Vec<String> {
    let next = resolve_globals(&ctx.config.get());
    let previous = *ctx.globals.read();
    if next == previous {
        return Vec::new();
    }
    let gs = app.global_shortcut();
    for (sc, label) in previous.labelled() {
        if let Err(e) = gs.unregister(sc) {
            // Not fatal: an unregister failure just means the OS never had it
            // (it lost the original registration race), and re-registering the
            // same chord below would then be a no-op anyway.
            tracing::debug!("unregistering '{}' failed: {}", label, e);
        }
    }
    *ctx.globals.write() = next;
    let failures = register_current(app, ctx);
    tracing::info!("global hotkeys re-registered from config");
    failures
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
