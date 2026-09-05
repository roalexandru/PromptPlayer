//! Tauri builder configuration — the assembly point for plugins, managed
//! state, IPC handlers, the tray icon, lifecycle hooks, and shortcuts.

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

    // Settings load first: `AppContext` needs `restore_armed` to decide the
    // initial armed state before `AppState` is constructed.
    let ctx = AppContext::with_settings(crate::settings::SettingsStore::shared());
    if ctx.armed_was_restored() {
        tracing::info!("armed state restored from settings (opt-in)");
    }

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
            // First run: copy examples into the writable library root. Loading
            // them in place made every edit try to write inside the bundle.
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

    // Hook spawns before Tauri so it owns its thread lifecycle; it reaches
    // FireService through a shared Arc<RwLock<Option<_>>> once one exists.
    let fire_holder: Arc<parking_lot::RwLock<Option<FireService>>> =
        Arc::new(parking_lot::RwLock::new(None));
    // Same pattern as fire_holder: the AppHandle doesn't exist until setup(),
    // but earlier closures need it.
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

    let on_literal_commit = {
        let h = app_handle_holder.clone();
        Arc::new(move |commit: char| {
            let h = h.clone();
            thread::Builder::new()
                .name("prompt-player-literal-commit".into())
                .spawn(move || match crate::inject::EnigoInjector::new() {
                    Ok(mut inj) => {
                        use crate::typer::Injector;
                        inj.press_backspace();
                        inj.type_char(commit);
                    }
                    Err(e) => {
                        tracing::error!("literal commit injector init failed: {:?}", e);
                        if let Some(app) = h.read().clone() {
                            telemetry::send(
                                &app,
                                TelemetryEvent::InjectionFailed {
                                    stage: crate::telemetry::InjectionStage::LiteralCommit,
                                },
                            );
                        }
                    }
                })
                .expect("spawn literal commit injector");
        })
    };

    // Via app_handle_holder: no AppHandle until setup(), which is well before
    // the user can type, so the dropped first milliseconds don't matter.
    let on_commit_observed = {
        let h = app_handle_holder.clone();
        Arc::new(move |matched: bool, index_size: usize| {
            if let Some(app) = h.read().clone() {
                telemetry::send(
                    &app,
                    TelemetryEvent::CommitObserved {
                        matched,
                        index_size_bucket: crate::telemetry::CountBucket::classify(index_size),
                    },
                );
            }
        })
    };

    // Aggregated by the tracker and flushed with the secure-input window, so
    // a burst of blocked triggers doesn't become a burst of events.
    let on_blocked_commit = {
        let tracker = ctx.secure_input.clone();
        Arc::new(move || tracker.note_blocked_commit())
    };

    let hook_callbacks = crate::hook::HookCallbacks {
        on_fire: on_fire.clone(),
        on_undo: on_undo.clone(),
        on_literal_commit: on_literal_commit.clone(),
        on_commit_observed: on_commit_observed.clone(),
        on_blocked_commit: on_blocked_commit.clone(),
    };

    let _hook = spawn_grabbing_hook(
        ctx.matcher.clone(),
        ctx.undo.clone(),
        ctx.state.clone(),
        hook_callbacks.clone(),
    );

    // §9.1 — Accessibility watcher (mac). The first spawn fails silently when
    // TCC hasn't approved this path+cdhash; this respawns without a restart.
    #[cfg(target_os = "macos")]
    {
        let matcher = ctx.matcher.clone();
        let undo = ctx.undo.clone();
        let app_state = ctx.state.clone();
        let cb = hook_callbacks.clone();
        let handle = app_handle_holder.clone();
        let attention = ctx.attention.clone();
        thread::Builder::new()
            .name("prompt-player-ax-watch".into())
            .spawn(move || {
                let mut last_alive = app_state.hook_alive();
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    let alive = app_state.hook_alive();
                    // Both directions: boot-time-only reporting left a repair
                    // invisible for the rest of a multi-week session.
                    if alive != last_alive {
                        last_alive = alive;
                        if let Some(app) = handle.read().clone() {
                            if attention.set_hook_dead(!alive) {
                                crate::tray_icon::refresh(&app);
                            }
                            telemetry::send(
                                &app,
                                TelemetryEvent::HookStateChanged {
                                    alive,
                                    accessibility_trusted: crate::tcc::is_accessibility_trusted(),
                                    reason: if alive {
                                        crate::telemetry::HookChangeReason::Installed
                                    } else {
                                        crate::telemetry::HookChangeReason::Died
                                    },
                                },
                            );
                        }
                    }
                    if alive || !crate::tcc::is_accessibility_trusted() {
                        continue;
                    }
                    tracing::info!("Accessibility granted — respawning keyboard hook");
                    if let Some(app) = handle.read().clone() {
                        telemetry::send(
                            &app,
                            TelemetryEvent::HookStateChanged {
                                alive: false,
                                accessibility_trusted: true,
                                reason: crate::telemetry::HookChangeReason::Respawn,
                            },
                        );
                    }
                    crate::hook::respawn_macos(
                        matcher.clone(),
                        undo.clone(),
                        app_state.clone(),
                        cb.clone(),
                    );
                }
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
            // In-app CRUD still reindexes via `reindex_after_mutation`; only
            // external file edits stop hot-reloading.
            tracing::error!(
                "library watcher failed to start ({}); external edits to {:?} \
                 won't hot-reload until restart",
                e,
                library_root
            );
        }
    }

    let ctx_for_setup = ctx.clone();
    let ctx_for_exit = ctx.clone();
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
        // MUST be first: a duplicate launch has to be intercepted before any
        // other plugin's setup registers a second tray icon and hook.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("duplicate launch detected — surfacing picker");
            // Same path as the shortcut/tray so the relaunch also captures
            // focus, refreshes the index, and positions on the cursor's screen.
            if let Some(ctx) = app.try_state::<AppContext>() {
                let ctx = ctx.inner().clone();
                let app2 = app.clone();
                let _ = app.run_on_main_thread(move || {
                    crate::commands::picker::summon_picker(
                        &app2,
                        &ctx,
                        crate::telemetry::PickerSource::Relaunch,
                        crate::commands::picker::FocusCapture::Take,
                    );
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

    // Per-state managed handles — every `tauri::State<'_, T>` parameter needs
    // its T here or Tauri panics. Kept in sync with `manage_state` by a test.
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
            commands::armed::reset_accessibility,
            commands::power::get_keep_awake,
            commands::power::toggle_keep_awake,
            commands::power::set_keep_awake_duration,
            commands::power::set_keep_awake_restore,
            commands::diagnostics::get_diagnostics,
            commands::diagnostics::run_self_test,
            commands::diagnostics::self_test_type,
            commands::diagnostics::open_diagnostics,
            commands::diagnostics::get_settings,
            commands::diagnostics::set_restore_armed,
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
            commands::updater::updater_announced,
            commands::updater::updater_dismiss,
            commands::library::capture_foreground_app,
            commands::library::expand_prompt_text,
            commands::library::import_prompt,
            commands::library::export_prompt,
            commands::shell::open_external,
        ])
        .setup(move |app| {
            // Bytes baked in — runtime paths differ between `cargo run` and the
            // bundle. `tray_icon::refresh` owns every later redraw.
            #[cfg(not(target_os = "windows"))]
            const TRAY_ICON_BYTES: &[u8] = include_bytes!("../../icons/tray-icon.png");
            #[cfg(target_os = "windows")]
            let tray_icon_bytes: &[u8] = crate::platform::windows::pick_tray_icon_bytes();
            #[cfg(not(target_os = "windows"))]
            let tray_icon_bytes: &[u8] = TRAY_ICON_BYTES;
            let tray_image = tauri::image::Image::from_bytes(tray_icon_bytes)
                .unwrap_or_else(|_| app.default_window_icon().unwrap().clone());
            let _tray = TrayIconBuilder::with_id(crate::tray_icon::TRAY_ID)
                .icon(tray_image)
                // `icon_as_template(true)` is mac-only behavior; on Windows
                // it's a no-op so leaving it unconditional is fine.
                .icon_as_template(true)
                .on_tray_icon_event(move |tray, event| {
                    use tauri::tray::{MouseButton, MouseButtonState};
                    // Both buttons open it, like every native menu-bar utility.
                    // macOS sends Right for Control-click, so that's covered too.
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

            // Windows-only: poll the OS theme and swap the tray icon. macOS
            // handles this via icon-as-template.
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

            // §9.1 first-run check. The PROMPTING variant registers us in the
            // Accessibility list, so the pane has a row with a real toggle.
            #[cfg(target_os = "macos")]
            {
                let trusted = crate::tcc::prompt_for_accessibility();
                // `hook::macos::spawn` flips `hook_alive` asynchronously, so an
                // immediate read is a false negative while the tap installs.
                let hook_alive = await_hook_settle(&ctx_for_setup, trusted);
                let exe_path = std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "<unknown>".into());
                // If a user reports "trigger doesn't fire", this one line says
                // whether to chase TCC, the tap, or the bundle.
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
                // Badge the tray so a dead hook is visible without opening
                // anything, then show diagnostics on the first broken run.
                ctx_for_setup.attention.set_hook_dead(!hook_alive);
                crate::tray_icon::refresh(app.handle());
                if !hook_alive && !ctx_for_setup.settings.get().setup_seen {
                    ctx_for_setup.settings.update(|s| s.setup_seen = true);
                    crate::commands::diagnostics::show(app.handle());
                }
            }

            // Windows has no TCC equivalent, so `accessibility_trusted` is a
            // constant true; the event still fires for cross-platform parity.
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

            // Secure-Input poller, 2s. One aggregated event per window instead
            // of one per rising edge, which was 91% of all telemetry.
            #[cfg(target_os = "macos")]
            {
                let app_for_secure = app.handle().clone();
                let tracker = ctx_for_setup.secure_input.clone();
                std::thread::Builder::new()
                    .name("prompt-player-secure-input".into())
                    .spawn(move || {
                        let mut last = false;
                        let mut window_start = std::time::Instant::now();
                        loop {
                            let now_active = crate::secure_input::is_active();
                            tracker.observe(now_active);
                            if now_active != last {
                                if now_active {
                                    tracing::warn!(
                                        "macOS Secure Input is ACTIVE — keyboard hook is BLOCKED"
                                    );
                                } else {
                                    tracing::info!("macOS Secure Input cleared");
                                }
                                last = now_active;
                            }
                            if window_start.elapsed() >= SECURE_INPUT_WINDOW {
                                window_start = std::time::Instant::now();
                                flush_secure_input(&app_for_secure, &tracker);
                            }
                            std::thread::sleep(std::time::Duration::from_secs(2));
                        }
                    })
                    .expect("spawn secure-input poll thread");
            }

            // Keep-awake auto-off. Cheap in-memory deadline check; the tray
            // label shows the countdown so the release isn't a surprise.
            {
                let app_for_power = app.handle().clone();
                let ctx_for_power = ctx_for_setup.clone();
                std::thread::Builder::new()
                    .name("prompt-player-keep-awake-expiry".into())
                    .spawn(move || loop {
                        std::thread::sleep(crate::power::EXPIRY_POLL_INTERVAL);
                        if ctx_for_power.power.expire_if_due() {
                            ctx_for_power.settings.update(|s| s.keep_awake = false);
                            telemetry::send(&app_for_power, TelemetryEvent::KeepAwakeExpired);
                            shortcuts::refresh_tray_popup(&app_for_power);
                        }
                    })
                    .expect("spawn keep-awake expiry thread");
            }

            // Independent of the hook on purpose: `CommitObserved` lives inside
            // it, so a dead hook also silences the instrumentation.
            {
                let app_for_beat = app.handle().clone();
                let ctx_for_beat = ctx_for_setup.clone();
                std::thread::Builder::new()
                    .name("prompt-player-heartbeat".into())
                    .spawn(move || loop {
                        std::thread::sleep(crate::telemetry::HEARTBEAT_INTERVAL);
                        telemetry::send(
                            &app_for_beat,
                            TelemetryEvent::Heartbeat {
                                hook_alive: ctx_for_beat.state.hook_alive(),
                                accessibility_trusted: crate::tcc::is_accessibility_trusted(),
                                armed: ctx_for_beat.state.is_armed(),
                                keep_awake: ctx_for_beat.power.is_enabled(),
                                prompts: crate::telemetry::CountBucket::classify(
                                    ctx_for_beat.prompts.len(),
                                ),
                            },
                        );
                    })
                    .expect("spawn heartbeat thread");
            }

            // Restore keep-awake only if the user opted in, always with a
            // fresh deadline so a persisted "on" can't resurrect a long session.
            {
                let s = ctx_for_setup.settings.get();
                if s.restore_keep_awake && s.keep_awake {
                    ctx_for_setup.power.set_for(true, s.keep_awake_mins);
                    tracing::info!("keep-awake restored for {} min", s.keep_awake_mins);
                }
            }

            // §13 — update poller, startup + every 6h. Manual checks use the
            // `updater_check` IPC and don't come through here.
            spawn_update_poller(app.handle().clone(), ctx_for_setup.clone());

            // Once per launch. Reports library shape rather than restating the
            // os/locale/version columns Aptabase already sends.
            let prompts = ctx_for_setup.prompts.snapshot();
            let enabled: Vec<_> = prompts.iter().filter(|p| p.enabled).collect();
            let trigger_count: usize = enabled.iter().map(|p| p.triggers.len()).sum();
            telemetry::send(
                app.handle(),
                TelemetryEvent::AppStarted {
                    prompts: crate::telemetry::CountBucket::classify(prompts.len()),
                    triggers: crate::telemetry::CountBucket::classify(trigger_count),
                    hotkeys: crate::telemetry::CountBucket::classify(
                        ctx_for_setup.hotkeys.read().len(),
                    ),
                    autostart: autostart_enabled(app.handle()),
                    armed_restored: ctx_for_setup.armed_was_restored(),
                },
            );

            tracing::info!(
                "Prompt Player started — armed={}",
                ctx_for_setup.state.is_armed()
            );
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |app, event| match event {
            tauri::RunEvent::ExitRequested { api, code, .. } => {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
            // Clean-shutdown marker. Its absence before the next `AppStarted`
            // is the only crash-vs-quit signal we have.
            tauri::RunEvent::Exit => {
                #[cfg(target_os = "macos")]
                flush_secure_input(app, &ctx_for_exit.secure_input);
                telemetry::send(
                    app,
                    TelemetryEvent::AppExiting {
                        uptime: crate::telemetry::DurationBucket::classify(
                            ctx_for_exit.started_at.elapsed(),
                        ),
                        fires: crate::telemetry::CountBucket::classify(
                            ctx_for_exit
                                .fire_count
                                .load(std::sync::atomic::Ordering::Relaxed),
                        ),
                    },
                );
                // The plugin flushes on Exit too, but ordering between plugin
                // handlers isn't guaranteed — make it explicit.
                telemetry::flush(app);
            }
            _ => {}
        });
}

/// How long a `SecureInputWindow` covers — six hours turns ~2,800 per-edge
/// events into a couple of dozen without losing the shape.
#[cfg(target_os = "macos")]
const SECURE_INPUT_WINDOW: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Emit one aggregated secure-input window, if anything happened in it.
#[cfg(target_os = "macos")]
fn flush_secure_input(app: &AppHandle, tracker: &crate::secure_input::SecureInputTracker) {
    let stats = tracker.drain();
    if stats.is_empty() {
        return;
    }
    telemetry::send(
        app,
        TelemetryEvent::SecureInputWindow {
            activations: crate::telemetry::CountBucket::classify(stats.activations as usize),
            active: crate::telemetry::DurationBucket::classify(stats.active),
            blocked_commits: crate::telemetry::CountBucket::classify(
                stats.blocked_commits as usize,
            ),
        },
    );
}

/// Wait briefly for the async tap install to flip `hook_alive`, so the boot
/// `success` flag isn't just a race with the tap thread.
#[cfg(target_os = "macos")]
fn await_hook_settle(ctx: &AppContext, trusted: bool) -> bool {
    if !trusted {
        return ctx.state.hook_alive();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(750);
    while std::time::Instant::now() < deadline {
        if ctx.state.hook_alive() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    ctx.state.hook_alive()
}

/// Whether launch-at-login is on. Reported on `AppStarted` because it's the
/// difference between "opened the app once" and "runs it every day".
fn autostart_enabled(app: &AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Poll the updater on startup and every 6h, emitting `update-available` for
/// the tray popup. Errors are reported but never kill the loop.
fn spawn_update_poller(app: tauri::AppHandle, ctx: AppContext) {
    use tauri::Emitter;
    use tauri_plugin_updater::UpdaterExt;
    /// At most one "nothing new" per day. The check stays 6-hourly; only the
    /// reporting is throttled, since 200 of 220 said nothing new.
    const NO_UPDATE_REPORT_INTERVAL: u64 = 24 * 60 * 60;

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
                        let dismissed =
                            ctx.settings.get().dismissed_update.as_deref() == Some(&update.version);
                        if !dismissed {
                            let payload = serde_json::json!({
                                "version": update.version,
                                "notes": update.body,
                            });
                            let _ = app.emit("update-available", payload);
                            // A row inside a popover nobody opens is why updates
                            // sat unseen for days; badge the always-visible icon.
                            if ctx.attention.set_update(true) {
                                crate::tray_icon::refresh(&app);
                            }
                        }
                        crate::telemetry::send(
                            &app,
                            crate::telemetry::TelemetryEvent::UpdateCheck { available: true },
                        );
                    }
                    Ok(None) => {
                        let now = crate::settings::now_unix();
                        let last = ctx.settings.get().last_no_update_report;
                        if now.saturating_sub(last) >= NO_UPDATE_REPORT_INTERVAL {
                            ctx.settings.update(|s| s.last_no_update_report = now);
                            crate::telemetry::send(
                                &app,
                                crate::telemetry::TelemetryEvent::UpdateCheck { available: false },
                            );
                        }
                    }
                    Err(e) => {
                        // Was log-and-drop, which made a broken updater
                        // indistinguishable from an up-to-date machine.
                        tracing::warn!("update check failed: {}", e);
                        crate::telemetry::send(
                            &app,
                            crate::telemetry::TelemetryEvent::UpdateCheckFailed {
                                stage: crate::telemetry::UpdateFailStage::Check,
                            },
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!("updater unavailable: {}", e);
                    crate::telemetry::send(
                        &app,
                        crate::telemetry::TelemetryEvent::UpdateCheckFailed {
                            stage: crate::telemetry::UpdateFailStage::Unavailable,
                        },
                    );
                }
            }
            // Spec §13: every 6h.
            tokio::time::sleep(std::time::Duration::from_secs(6 * 60 * 60)).await;
        }
    });
}

/// Copy bundled `.pp.md` examples into the library root on first run.
/// Idempotent — skips existing files and non-`.pp.md`. Returns the count.
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

/// Reindex after an in-app mutation. Can't wait for the file watcher: it may
/// have failed to start, and a deleted prompt must stop firing immediately.
pub(crate) fn reindex_after_mutation(app: &AppHandle, ctx: &AppContext) {
    rebuild_match_index(ctx);
    shortcuts::rebuild_prompt_hotkeys(app, ctx);
    shortcuts::refresh_tray_popup(app);
}

/// The bundled `prompts-examples` dir inside the .app/.msi. None under
/// `cargo run` or in tests, where the caller falls back to CWD.
fn first_run_bundled_examples() -> Option<std::path::PathBuf> {
    // Resources sit beside the executable. No AppHandle here — this runs
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

/// Emit `src/lib/ipc.gen.ts` from the Specta-annotated commands. Debug only,
/// so path resolution can never block a release build.
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
        crate::commands::armed::reset_accessibility,
        crate::commands::power::get_keep_awake,
        crate::commands::power::toggle_keep_awake,
        crate::commands::power::set_keep_awake_duration,
        crate::commands::power::set_keep_awake_restore,
        crate::commands::diagnostics::get_diagnostics,
        crate::commands::diagnostics::run_self_test,
        crate::commands::diagnostics::self_test_type,
        crate::commands::diagnostics::open_diagnostics,
        crate::commands::diagnostics::get_settings,
        crate::commands::diagnostics::set_restore_armed,
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
        crate::commands::updater::updater_announced,
        crate::commands::updater::updater_dismiss,
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

#[cfg(all(test, debug_assertions))]
mod bindings_tests {
    /// Regenerate `ipc.gen.ts` and fail if the checked-in copy was stale.
    /// The regenerated file is left in place, so the fix is to commit it.
    #[test]
    fn checked_in_bindings_match_the_generator() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("manifest parent")
            .join("src/lib/ipc.gen.ts");
        let before = std::fs::read_to_string(&path).unwrap_or_default();
        super::generate_typescript_bindings().expect("specta export");
        let after = std::fs::read_to_string(&path).expect("read regenerated bindings");
        assert_eq!(
            before, after,
            "src/lib/ipc.gen.ts was stale and has been regenerated — commit it"
        );
    }
}

fn rebuild_match_index(ctx: &AppContext) {
    let prompts = ctx.prompts.read();
    let mut entries = Vec::new();
    for p in prompts.iter() {
        // Indexing a disabled prompt would eat the user's `>` mid-demo and
        // expand nothing.
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
    // One line so a Console.app subsystem filter catches the trigger count.
    // Empty here = nothing can ever match, whatever the hook is doing.
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
        // Plain NSWindows need `CanJoinAllSpaces | FullScreenAuxiliary`, or an
        // `.accessory` app anchors them to the launch Space and they look gone.
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
            // Mica on Win11, Acrylic on Win10. Losing either is cosmetic only.
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

/// All `manage()` calls, generic over the runtime so the tests can reuse it.
/// A new type goes here AND in `run()`'s inline block; a test asserts both.
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
