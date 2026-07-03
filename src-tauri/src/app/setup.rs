//! Tauri builder configuration. This is the assembly point for plugins,
//! managed state, IPC handlers, the tray icon, lifecycle hooks, and the
//! global shortcut registration.

use crate::app::context::AppContext;
use crate::app::{lifecycle, shortcuts, FireService};
use crate::commands;
use crate::hook::spawn_grabbing_hook;
use crate::matcher::TriggerEntry;
use crate::prompts::library;
use crate::telemetry::{self, TelemetryEvent};
use std::sync::Arc;
use std::thread;
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

#[cfg(target_os = "macos")]
use crate::platform::macos::OutsideClickMonitor;
// Windows doesn't have an OutsideClickMonitor — its tray uses a native
// HMENU via `TrackPopupMenuEx`, where the OS owns dismissal entirely.

/// Build and run the Tauri application. Called once from `main.rs`.
pub fn run() {
    crate::telemetry::init();

    let ctx = AppContext::new();

    // Phase 5: load prompts from the library directory (with hot reload).
    let library_root = library::default_library_root()
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("prompts-examples"));
    if let Err(e) = std::fs::create_dir_all(&library_root) {
        tracing::warn!("could not create library dir {:?}: {}", library_root, e);
    }
    {
        let (loaded, errs) = library::load_all(&library_root);
        for e in &errs {
            tracing::warn!("prompt parse: {}", e);
        }
        if loaded.is_empty() {
            // First run (empty library): COPY the bundled examples into the
            // user's writable library root, then load from there. Loading them
            // in place from the bundle was a bug — `source_path` pointed inside
            // the signed .app / Program Files, so any edit/toggle/delete tried
            // to write into the bundle (breaks code-signing on macOS, denied
            // under Program Files on Windows) and the watcher (which watches
            // library_root, not the bundle) never saw the changes; worse, the
            // first user-created prompt triggered a load-all that dropped every
            // bundled example from memory.
            let bundle = first_run_bundled_examples().or_else(|| {
                let cwd = std::env::current_dir().ok()?.join("prompts-examples");
                cwd.exists().then_some(cwd)
            });
            if let Some(bundle) = bundle {
                if bundle != library_root {
                    let copied = copy_bundled_examples(&bundle, &library_root);
                    tracing::info!(
                        "first run: copied {} bundled example(s) into {:?}",
                        copied,
                        library_root
                    );
                }
            }
            let (loaded2, errs2) = library::load_all(&library_root);
            for e in &errs2 {
                tracing::warn!("prompt parse (post-bootstrap): {}", e);
            }
            ctx.prompts.replace_all(loaded2);
        } else {
            ctx.prompts.replace_all(loaded);
        }
        tracing::info!(
            "loaded {} prompt(s) from {:?}",
            ctx.prompts.len(),
            library_root
        );
        rebuild_match_index(&ctx);
    }

    // Spawn the keyboard hook BEFORE Tauri startup so it owns its own thread
    // lifecycle. The hook calls into the FireService once the AppHandle is
    // available — we plumb it through via a shared Arc<RwLock<Option<...>>>.
    let fire_holder: Arc<parking_lot::RwLock<Option<FireService>>> =
        Arc::new(parking_lot::RwLock::new(None));
    // The AppHandle isn't constructed until inside Tauri's setup() callback,
    // but we need to capture it from closures (telemetry callback, hot-reload
    // watcher) that run earlier or on different threads. Same pattern as
    // fire_holder. Declared up here so the on_commit_observed closure below
    // can capture it before Tauri builds.
    let app_handle_holder: Arc<parking_lot::RwLock<Option<AppHandle>>> =
        Arc::new(parking_lot::RwLock::new(None));

    let on_fire = {
        let h = fire_holder.clone();
        Arc::new(move |candidate_ids: Vec<String>, typed_form: String| {
            if let Some(svc) = h.read().clone() {
                svc.fire_from_trigger(candidate_ids, typed_form);
            }
        })
    };

    let on_undo = {
        let h = fire_holder.clone();
        Arc::new(move || {
            if let Some(svc) = h.read().clone() {
                svc.run_undo();
            }
        })
    };

    let on_literal_commit = Arc::new(move |commit: char| {
        thread::Builder::new()
            .name("prompt-player-literal-commit".into())
            .spawn(move || match crate::inject::EnigoInjector::new() {
                Ok(mut inj) => {
                    use crate::typer::Injector;
                    inj.press_backspace();
                    inj.type_char(commit);
                }
                Err(e) => tracing::error!("literal commit injector init failed: {:?}", e),
            })
            .expect("spawn literal commit injector");
    });

    // Telemetry callback for the commit-char path. Plumbs through
    // app_handle_holder because the AppHandle isn't constructed yet at this
    // point (Tauri builds it inside setup()). We do nothing if the AppHandle
    // isn't ready; that only applies to events fired in the milliseconds
    // before Tauri's setup() runs, which is well before the user can type.
    let on_commit_observed = {
        let h = app_handle_holder.clone();
        Arc::new(move |matched: bool, index_size: usize| {
            if let Some(app) = h.read().clone() {
                telemetry::send(
                    &app,
                    TelemetryEvent::CommitObserved {
                        matched,
                        index_size_bucket: crate::telemetry::IndexSizeBucket::classify(index_size),
                    },
                );
            }
        })
    };

    let _hook = spawn_grabbing_hook(
        ctx.matcher.clone(),
        ctx.undo.clone(),
        ctx.state.clone(),
        on_fire.clone(),
        on_undo.clone(),
        on_literal_commit.clone(),
        on_commit_observed.clone(),
    );

    // §9.1 — Accessibility-status watcher (mac only). The first hook spawn
    // fails silently if Accessibility wasn't pre-granted (common on a fresh
    // DMG install at /Applications/Prompt Player.app — TCC keys approval by
    // path+cdhash for unsigned apps and the new path has none). When the user
    // finally toggles Accessibility on in System Settings, this poller detects
    // the transition and respawns the hook without requiring an app restart.
    //
    // Kept narrow: 5s cadence, only does work on transitions, exits as soon
    // as the hook reports alive (we re-arm only if it dies again).
    #[cfg(target_os = "macos")]
    {
        let matcher = ctx.matcher.clone();
        let undo = ctx.undo.clone();
        let app_state = ctx.state.clone();
        let on_fire2 = on_fire.clone();
        let on_undo2 = on_undo.clone();
        let on_literal2 = on_literal_commit.clone();
        let on_commit2 = on_commit_observed.clone();
        thread::Builder::new()
            .name("prompt-player-ax-watch".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                if app_state.hook_alive() {
                    continue;
                }
                if !crate::tcc::is_accessibility_trusted() {
                    continue;
                }
                tracing::info!("Accessibility granted — respawning keyboard hook");
                crate::hook::respawn_macos(
                    matcher.clone(),
                    undo.clone(),
                    app_state.clone(),
                    on_fire2.clone(),
                    on_undo2.clone(),
                    on_literal2.clone(),
                    on_commit2.clone(),
                );
            })
            .expect("spawn ax watch thread");
    }

    // Hot-reload watcher: re-load prompts on file changes, rebuild trigger
    // index + per-prompt hotkeys, refresh tray popup.
    match library::watch(&library_root) {
        Ok(watcher) => {
            let ctx2 = ctx.clone();
            let handle2 = app_handle_holder.clone();
            let root2 = library_root.clone();
            thread::Builder::new()
                .name("prompt-player-watch".into())
                .spawn(move || loop {
                    if library::drain_events(&watcher, std::time::Duration::from_millis(500)) {
                        let (loaded, errs) = library::load_all(&root2);
                        for e in errs {
                            tracing::warn!("hot-reload parse: {}", e);
                        }
                        ctx2.prompts.replace_all(loaded);
                        rebuild_match_index(&ctx2);
                        if let Some(h) = handle2.read().clone() {
                            shortcuts::rebuild_prompt_hotkeys(&h, &ctx2);
                            shortcuts::refresh_tray_popup(&h);
                        }
                        tracing::info!("library hot-reloaded — {} prompt(s)", ctx2.prompts.len());
                    }
                })
                .expect("spawn watch thread");
        }
        Err(e) => {
            // Don't fail silently: with no watcher, on-disk edits won't
            // hot-reload. The IPC mutation commands still reindex directly
            // (see `reindex_after_mutation`), so in-app CRUD stays correct —
            // but external file edits won't be picked up until restart.
            tracing::error!(
                "library watcher failed to start ({}); external edits to {:?} \
                 won't hot-reload until restart",
                e,
                library_root
            );
        }
    }

    let ctx_for_setup = ctx.clone();
    let fire_holder_for_setup = fire_holder.clone();
    let app_handle_holder_for_setup = app_handle_holder.clone();

    // Type contract: generate src/lib/ipc.gen.ts in debug builds at startup.
    // Runs before the Tauri builder so the frontend has types ready on load.
    #[cfg(debug_assertions)]
    {
        if let Err(e) = generate_typescript_bindings() {
            tracing::warn!("specta TS bindings generation failed: {}", e);
        }
    }

    let mut builder = tauri::Builder::default()
        // Single-instance MUST be the first plugin so a duplicate launch is
        // intercepted before any other plugin's setup hook runs — that's
        // what prevents the second process from registering its own tray
        // icon (the reported Windows bug: launching from Start menu while
        // already running adds a duplicate tray icon and a duplicate hook
        // listener). The callback fires in the FIRST process with the
        // second launcher's argv/cwd; we use it to surface the picker so
        // "launch the app" still feels like it did something.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("duplicate launch detected — surfacing picker");
            // Route through the same `summon_picker` path as the shortcut/tray
            // so the relaunch also captures focus, refreshes the search index,
            // and positions on the cursor's screen — not a bare show() that
            // skipped all three.
            if let Some(ctx) = app.try_state::<AppContext>() {
                let ctx = ctx.inner().clone();
                let app2 = app.clone();
                let _ = app.run_on_main_thread(move || {
                    crate::commands::picker::summon_picker(&app2, &ctx);
                });
            } else {
                crate::commands::picker::show_picker_window(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_aptabase::Builder::new(crate::telemetry::APTABASE_KEY).build());

    // Per-state managed handles. Each typed `tauri::State<'_, T>` IPC
    // parameter needs its T registered here, otherwise Tauri panics with
    // "state not managed for field". AppContext also references most of
    // these via Arc, so we manage them independently for the commands
    // that take a narrow handle (PromptStore, AppState).
    builder = builder
        .manage(ctx.state.clone())
        .manage(ctx.prompts.clone())
        .manage(ctx.clone());
    #[cfg(target_os = "macos")]
    {
        builder = builder.manage(OutsideClickMonitor::shared());
    }

    builder
        .invoke_handler(tauri::generate_handler![
            commands::armed::get_armed,
            commands::armed::toggle_armed,
            commands::armed::kill,
            commands::armed::is_playing,
            commands::armed::is_hook_alive,
            commands::armed::open_accessibility_settings,
            commands::prompts::list_prompts,
            commands::prompts::library_root,
            commands::prompts::save_prompt,
            commands::prompts::create_prompt,
            commands::prompts::delete_prompt,
            commands::prompts::set_prompt_enabled,
            commands::prompts::set_prompt_pinned,
            commands::picker::picker_open,
            commands::picker::picker_search,
            commands::picker::picker_select,
            commands::picker::picker_dismiss,
            commands::tray::tray_open,
            commands::tray::tray_quit,
            commands::tray::tray_popup_hide,
            commands::tray::tray_fire_prompt,
            commands::updater::updater_current_version,
            commands::updater::updater_check,
            commands::updater::updater_install,
            commands::library::capture_foreground_app,
            commands::library::expand_prompt_text,
            commands::library::import_prompt,
            commands::library::export_prompt,
            commands::shell::open_external,
        ])
        .setup(move |app| {
            // Tray icon — left-click toggles the WiFi-style stay-open popover.
            // Embedded at compile-time: runtime path resolution is brittle
            // (cargo/tauri-dev CWD differs from the packaged .app's resources
            // dir, and the bundle doesn't ship icons/ as a resource). Bytes
            // baked into the binary work in both contexts.
            //
            // Per-OS icon picking:
            // - macOS: monochrome glyph + `icon_as_template(true)` lets AppKit
            //   tint the icon to match the menubar (white on dark, black on
            //   light) automatically. The shipped `tray-icon.png` is a black
            //   luminance mask.
            // - Windows: no template-image concept. We ship two pre-rendered
            //   variants (light/dark) and pick the right one at startup based
            //   on the current `SystemUsesLightTheme` registry value, then
            //   spawn a watcher that swaps the icon on theme change.
            #[cfg(not(target_os = "windows"))]
            const TRAY_ICON_BYTES: &[u8] = include_bytes!("../../icons/tray-icon.png");
            #[cfg(target_os = "windows")]
            let tray_icon_bytes: &[u8] = crate::platform::windows::pick_tray_icon_bytes();
            #[cfg(not(target_os = "windows"))]
            let tray_icon_bytes: &[u8] = TRAY_ICON_BYTES;
            let tray_image = tauri::image::Image::from_bytes(tray_icon_bytes)
                .unwrap_or_else(|_| app.default_window_icon().unwrap().clone());
            let _tray = TrayIconBuilder::with_id("main")
                .icon(tray_image)
                // `icon_as_template(true)` is mac-only behavior; on Windows
                // it's a no-op so leaving it unconditional is fine.
                .icon_as_template(true)
                .on_tray_icon_event(move |tray, event| {
                    use tauri::tray::{MouseButton, MouseButtonState};
                    // Both left- and right-click open the popover. Native menu-bar
                    // utilities (Things, Bartender, 1Password) all do this so the
                    // user never has to remember "wrong button". macOS sends Right
                    // for Control-click too, so this also covers that path.
                    if let tauri::tray::TrayIconEvent::Click {
                        button: MouseButton::Left | MouseButton::Right,
                        button_state: MouseButtonState::Down,
                        rect,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        crate::commands::tray::toggle_popup(app, rect);
                    }
                })
                .build(app)?;

            // Windows-only: poll the OS theme every 5s and swap the tray
            // icon when the user toggles light/dark in Settings. macOS
            // handles this for us via icon-as-template.
            #[cfg(target_os = "windows")]
            crate::platform::windows::install_tray_theme_watcher(app.handle().clone());

            // FireService — needs an AppHandle, so it's constructed at setup time.
            let fire = FireService::new(ctx_for_setup.clone(), app.handle().clone());
            *fire_holder_for_setup.write() = Some(fire.clone());

            shortcuts::register(app, ctx_for_setup.clone(), fire)?;
            *app_handle_holder_for_setup.write() = Some(app.handle().clone());
            shortcuts::rebuild_prompt_hotkeys(&app.handle().clone(), &ctx_for_setup);

            // Apply window chrome, install lifecycle handlers.
            apply_window_chrome(app);
            lifecycle::install(app);

            // Tray-only / menu-bar utility behavior.
            #[cfg(target_os = "macos")]
            crate::platform::macos::nsworkspace::set_accessory_activation_policy();

            // §9.1 first-run permission check on macOS. Use the PROMPTING
            // variant so this app gets registered in the Accessibility list
            // (the system pane is empty for unsigned apps on first launch
            // unless we explicitly prompt — that's how AppleScript editors,
            // BetterTouchTool, etc. all bootstrap their entries). After the
            // prompt, open Settings so the user lands on the right pane with
            // a row that actually has a toggle.
            #[cfg(target_os = "macos")]
            {
                let trusted = crate::tcc::prompt_for_accessibility();
                let hook_alive = ctx_for_setup.state.hook_alive();
                let exe_path = std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "<unknown>".into());
                // Single-line state dump of the boot decision triangle:
                // (a) Accessibility check, (b) hook install, (c) binary path.
                // If a user reports "trigger doesn't fire" this line tells us
                // immediately whether to chase TCC, the tap, or the bundle.
                tracing::info!(
                    "boot state: accessibility_trusted={} hook_alive={} exe={}",
                    trusted,
                    hook_alive,
                    exe_path
                );
                telemetry::send(
                    app.handle(),
                    TelemetryEvent::HookInstallResult {
                        success: hook_alive,
                        accessibility_trusted: trusted,
                    },
                );
                if !trusted {
                    tracing::warn!(
                        "macOS Accessibility permission not granted — opening System Settings pane"
                    );
                    crate::tcc::open_accessibility_settings();
                }
            }

            // Windows doesn't gate keyboard hooks behind a permission like TCC —
            // SetWindowsHookEx (via rdev::grab) just works for any user-mode
            // process. We still emit a HookInstallResult so dashboards have
            // parity across platforms; `accessibility_trusted` is hard-coded
            // true since the concept doesn't apply.
            #[cfg(target_os = "windows")]
            {
                let hook_alive = ctx_for_setup.state.hook_alive();
                let exe_path = std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "<unknown>".into());
                tracing::info!("boot state: hook_alive={} exe={}", hook_alive, exe_path);
                telemetry::send(
                    app.handle(),
                    TelemetryEvent::HookInstallResult {
                        success: hook_alive,
                        accessibility_trusted: true,
                    },
                );
            }

            // Secure-Input poller — 2s cadence (was 500ms; battery friendlier).
            // Surfaces transitions only — no per-poll log spam.
            #[cfg(target_os = "macos")]
            {
                let app_for_secure = app.handle().clone();
                std::thread::Builder::new()
                    .name("prompt-player-secure-input".into())
                    .spawn(move || {
                        let mut last = false;
                        loop {
                            let now_active = crate::secure_input::is_active();
                            if now_active != last {
                                if now_active {
                                    tracing::warn!(
                                        "macOS Secure Input is ACTIVE — keyboard hook is BLOCKED"
                                    );
                                    telemetry::send(
                                        &app_for_secure,
                                        TelemetryEvent::SecureInputDetected,
                                    );
                                } else {
                                    tracing::info!("macOS Secure Input cleared");
                                }
                                last = now_active;
                            }
                            std::thread::sleep(std::time::Duration::from_secs(2));
                        }
                    })
                    .expect("spawn secure-input poll thread");
            }

            // §13 — auto-update poller. Checks on startup and every 6h.
            // Emits a `update-available` event with `{ version, notes }` when
            // a new release is published; the frontend listens for it on the
            // tray-popup window and surfaces an "Install update vX.Y.Z" entry.
            // Manual checks (user clicks "Check for updates…") use the
            // `updater_check` IPC command and do NOT go through this poller.
            spawn_update_poller(app.handle().clone());

            // App-started telemetry event — once per launch.
            let locale = sys_locale::get_locale().unwrap_or_else(|| "en-US".into());
            telemetry::send(
                app.handle(),
                TelemetryEvent::AppStarted {
                    version: env!("CARGO_PKG_VERSION"),
                    os: if cfg!(target_os = "macos") {
                        "macos"
                    } else if cfg!(target_os = "windows") {
                        "windows"
                    } else {
                        "other"
                    },
                    locale,
                    profile_in_use: "sales-engineer",
                },
            );

            tracing::info!("Prompt Player started — disarmed (per §10.1)");
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}

/// Poll the updater endpoint on startup and every 6h. Emits a frontend
/// `update-available` event when a new release is found, so the tray popup
/// can surface an "Install update vX.Y.Z" entry without blocking on its own
/// check call. Errors are logged and swallowed — a transient network blip
/// shouldn't kill the poller.
fn spawn_update_poller(app: tauri::AppHandle) {
    use tauri::Emitter;
    use tauri_plugin_updater::UpdaterExt;
    tauri::async_runtime::spawn(async move {
        // Small initial delay so the poller doesn't race the rest of startup.
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        loop {
            match app.updater() {
                Ok(updater) => match updater.check().await {
                    Ok(Some(update)) => {
                        tracing::info!(
                            "update available: {} → {}",
                            env!("CARGO_PKG_VERSION"),
                            update.version
                        );
                        let payload = serde_json::json!({
                            "version": update.version,
                            "notes": update.body,
                        });
                        let _ = app.emit("update-available", payload);
                        crate::telemetry::send(
                            &app,
                            crate::telemetry::TelemetryEvent::UpdateCheck {
                                available: true,
                                current_version: env!("CARGO_PKG_VERSION"),
                            },
                        );
                    }
                    Ok(None) => {
                        crate::telemetry::send(
                            &app,
                            crate::telemetry::TelemetryEvent::UpdateCheck {
                                available: false,
                                current_version: env!("CARGO_PKG_VERSION"),
                            },
                        );
                    }
                    Err(e) => tracing::warn!("update check failed: {}", e),
                },
                Err(e) => tracing::warn!("updater unavailable: {}", e),
            }
            // Spec §13: every 6h.
            tokio::time::sleep(std::time::Duration::from_secs(6 * 60 * 60)).await;
        }
    });
}

/// Copy bundled `.pp.md` example files into the user's library root on first
/// run. Skips any file that already exists (idempotent) and non-`.pp.md`
/// files (e.g. the examples README). Returns the count copied.
fn copy_bundled_examples(src: &std::path::Path, dst: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(src) else {
        return 0;
    };
    let mut count = 0;
    for entry in entries.flatten() {
        let p = entry.path();
        let is_pp = p
            .file_name()
            .and_then(|f| f.to_str())
            .map(|s| s.ends_with(".pp.md"))
            .unwrap_or(false);
        if !p.is_file() || !is_pp {
            continue;
        }
        let Some(name) = p.file_name() else { continue };
        let target = dst.join(name);
        if target.exists() {
            continue;
        }
        match std::fs::copy(&p, &target) {
            Ok(_) => count += 1,
            Err(e) => tracing::warn!("failed to copy bundled example {:?}: {}", p, e),
        }
    }
    count
}

/// Reload the matcher index, per-prompt hotkeys, and tray popup after an
/// in-app library mutation (save/create/delete/enable/pin/import). The
/// hot-reload watcher does this on external file edits, but in-app CRUD must
/// not depend on the watcher (it may have failed to start, and a freshly
/// deleted prompt must stop firing immediately rather than after the next
/// filesystem event).
pub(crate) fn reindex_after_mutation(app: &AppHandle, ctx: &AppContext) {
    rebuild_match_index(ctx);
    shortcuts::rebuild_prompt_hotkeys(app, ctx);
    shortcuts::refresh_tray_popup(app);
}

/// Look up the bundled `prompts-examples` directory in the .app's Resources
/// folder. Returns None when running from `cargo run` (CWD-relative path is
/// used as fallback) or in test contexts.
fn first_run_bundled_examples() -> Option<std::path::PathBuf> {
    // The bundled resources directory in a .app/.msi is sibling to the
    // executable. We avoid taking a Tauri AppHandle here because this runs
    // before the Tauri builder.
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    // macOS: <bundle>/Contents/Resources/_up_/prompts-examples
    // (Tauri prefixes "../" relative paths with `_up_/`).
    let mac = dir.join("../Resources/_up_/prompts-examples");
    if mac.exists() {
        return Some(mac);
    }
    // Windows: <install>/resources/_up_/prompts-examples
    let win = dir.join("resources/_up_/prompts-examples");
    if win.exists() {
        return Some(win);
    }
    None
}

/// Emit `src/lib/ipc.gen.ts` from the Specta-annotated commands. Runs only
/// in debug builds (so the produced file is in source control as a
/// developer convenience but never blocks a release build if the path
/// resolution differs).
#[cfg(debug_assertions)]
fn generate_typescript_bindings() -> Result<(), String> {
    use specta_typescript::Typescript;
    use tauri_specta::{collect_commands, Builder};

    let builder = Builder::<tauri::Wry>::new().commands(collect_commands![
        crate::commands::armed::get_armed,
        crate::commands::armed::toggle_armed,
        crate::commands::armed::kill,
        crate::commands::armed::is_playing,
        crate::commands::armed::is_hook_alive,
        crate::commands::armed::open_accessibility_settings,
        crate::commands::prompts::list_prompts,
        crate::commands::prompts::library_root,
        crate::commands::prompts::save_prompt,
        crate::commands::prompts::create_prompt,
        crate::commands::prompts::delete_prompt,
        crate::commands::prompts::set_prompt_enabled,
        crate::commands::prompts::set_prompt_pinned,
        crate::commands::picker::picker_open,
        crate::commands::picker::picker_search,
        crate::commands::picker::picker_select,
        crate::commands::picker::picker_dismiss,
        crate::commands::tray::tray_open,
        crate::commands::tray::tray_quit,
        crate::commands::tray::tray_popup_hide,
        crate::commands::tray::tray_fire_prompt,
        crate::commands::updater::updater_current_version,
        crate::commands::updater::updater_check,
        crate::commands::updater::updater_install,
        crate::commands::library::capture_foreground_app,
        crate::commands::library::expand_prompt_text,
        crate::commands::library::import_prompt,
        crate::commands::library::export_prompt,
        crate::commands::shell::open_external,
    ]);

    // Resolve the workspace root reliably from CARGO_MANIFEST_DIR (baked in
    // at compile time via env!()).
    let manifest = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest)
        .parent()
        .ok_or("manifest has no parent")?
        .join("src/lib/ipc.gen.ts");

    builder
        .export(
            Typescript::default()
                .header("// AUTO-GENERATED. Do not edit. `cargo run` regenerates.\n"),
            &path,
        )
        .map_err(|e| format!("export: {}", e))?;
    tracing::info!("specta exported TS bindings → {:?}", path);
    Ok(())
}

fn rebuild_match_index(ctx: &AppContext) {
    let prompts = ctx.prompts.read();
    let mut entries = Vec::new();
    for p in prompts.iter() {
        // Skip disabled prompts: indexing them would suppress the user's
        // commit char (and pop their trigger from the ring) even though
        // nothing fires — eating the `>` mid-demo with no expansion.
        if !p.enabled {
            continue;
        }
        for t in &p.triggers {
            entries.push(TriggerEntry {
                canonical: t.to_lowercase(),
                prompt_id: p.id.clone(),
                word_count: t.split_whitespace().count(),
                commit_char: p.commit_char,
            });
        }
    }
    let entry_count = entries.len();
    let skipped = ctx.matcher.rebuild_index(entries);
    if skipped > 0 {
        tracing::warn!("matcher rebuild skipped {} duplicate trigger(s)", skipped);
    }
    // Single-line diagnostic so a Console.app filter on subsystem catches
    // the actual trigger count without needing to grep across multiple lines.
    // Empty index here = no triggers will ever match, regardless of hook state.
    tracing::info!(
        "matcher index rebuilt: prompts={} triggers={}",
        prompts.len(),
        entry_count
    );
}

fn apply_window_chrome(app: &tauri::App) {
    for label in ["library", "picker", "tray-popup", "about"] {
        if let Some(w) = app.get_webview_window(label) {
            #[cfg(target_os = "macos")]
            apply_macos_chrome(label, &w);
            #[cfg(target_os = "windows")]
            apply_windows_chrome(label, &w);
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            let _ = (label, w);
        }
    }
}

#[cfg(target_os = "macos")]
fn apply_macos_chrome(label: &str, w: &tauri::WebviewWindow) {
    use crate::platform::macos::{
        configure_picker_window, configure_popover_window, make_window_space_neutral,
    };
    use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
    match label {
        "picker" => {
            let _ = apply_vibrancy(
                w,
                NSVisualEffectMaterial::HudWindow,
                Some(NSVisualEffectState::Active),
                Some(12.0),
            );
            configure_picker_window(w);
        }
        "tray-popup" => {
            let _ = apply_vibrancy(
                w,
                NSVisualEffectMaterial::HudWindow,
                Some(NSVisualEffectState::Active),
                Some(10.0),
            );
            configure_popover_window(w);
        }
        // Library and About are plain NSWindow (not panels). Both need
        // `CanJoinAllSpaces | FullScreenAuxiliary` so they surface on
        // whatever Space is current when shown — without this, `.accessory`
        // apps anchor regular windows to the launch Space and "About"
        // clicked from the tray on a different Space looks invisible.
        "library" | "about" => {
            make_window_space_neutral(w);
        }
        _ => {}
    }
}

#[cfg(target_os = "windows")]
fn apply_windows_chrome(label: &str, w: &tauri::WebviewWindow) {
    use crate::platform::windows::{configure_picker_window, configure_popover_window};
    use window_vibrancy::{apply_acrylic, apply_mica};
    match label {
        "picker" => {
            // Try Mica first (Win11). Fall back to Acrylic on Win10 / older.
            // Both produce a translucent vibrancy similar to NSVisualEffectView's
            // HudWindow material; loss of either is purely cosmetic.
            if apply_mica(w, Some(true)).is_err() {
                let _ = apply_acrylic(w, Some((18, 18, 22, 160)));
            }
            configure_picker_window(w);
        }
        "tray-popup" => {
            if apply_mica(w, Some(true)).is_err() {
                let _ = apply_acrylic(w, Some((18, 18, 22, 160)));
            }
            configure_popover_window(w);
        }
        // Library — no chrome, no vibrancy. CSS backdrop-filter
        // gives inner panels glass in the webview.
        _ => {}
    }
}

/// Apply ALL `manage()` calls for the app. Generic over the Tauri runtime so
/// the integration smoke test in `tests/ipc_registry.rs` can reuse it
/// against `MockRuntime`.
///
/// Production `run()` calls this *and* the matching inline `.manage()` block
/// is kept in sync via `tests/ipc_registry.rs::manage_state_inline_matches_helper`.
/// Adding a managed type means: (a) edit this function, (b) edit the inline
/// block in `run()`, (c) `cargo test` re-asserts they match.
pub fn manage_state<R: tauri::Runtime>(
    builder: tauri::Builder<R>,
    ctx: AppContext,
) -> tauri::Builder<R> {
    #[allow(unused_mut)] // Windows path doesn't reassign — the `mut` is for the Mac branch below.
    let mut builder = builder
        .manage(ctx.state.clone())
        .manage(ctx.prompts.clone())
        .manage(ctx);
    #[cfg(target_os = "macos")]
    {
        builder = builder.manage(OutsideClickMonitor::shared());
    }
    builder
}
