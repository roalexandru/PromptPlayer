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
#[cfg(target_os = "windows")]
use crate::platform::windows::OutsideClickMonitor;

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
            // Bootstrap from the bundled `prompts-examples` resource on
            // first run. We try the bundled resource first; falling back
            // to the CWD-relative path (dev convenience).
            let bootstrap = first_run_bundled_examples()
                .unwrap_or_else(|| std::env::current_dir().unwrap().join("prompts-examples"));
            if bootstrap != library_root && bootstrap.exists() {
                let (l2, _) = library::load_all(&bootstrap);
                ctx.prompts.replace_all(l2);
            }
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

    let _hook = spawn_grabbing_hook(
        ctx.matcher.clone(),
        ctx.undo.clone(),
        ctx.state.clone(),
        on_fire,
        on_undo,
    );

    // Hot-reload watcher: re-load prompts on file changes, rebuild trigger
    // index + per-prompt hotkeys, refresh tray popup.
    let app_handle_holder: Arc<parking_lot::RwLock<Option<AppHandle>>> =
        Arc::new(parking_lot::RwLock::new(None));
    if let Ok(watcher) = library::watch(&library_root) {
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
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_os::init())
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
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        builder = builder.manage(OutsideClickMonitor::shared());
    }

    builder
        .invoke_handler(tauri::generate_handler![
            commands::armed::get_armed,
            commands::armed::toggle_armed,
            commands::armed::kill,
            commands::prompts::list_prompts,
            commands::prompts::library_root,
            commands::prompts::save_prompt,
            commands::prompts::create_prompt,
            commands::prompts::delete_prompt,
            commands::prompts::set_prompt_enabled,
            commands::picker::picker_open,
            commands::picker::picker_search,
            commands::picker::picker_select,
            commands::picker::picker_dismiss,
            commands::tray::tray_open,
            commands::tray::tray_quit,
            commands::tray::tray_popup_hide,
        ])
        .setup(move |app| {
            // Tray icon — left-click toggles the WiFi-style stay-open popover.
            let tray_icon_path = std::env::current_dir()
                .unwrap()
                .join("src-tauri/icons/tray-icon.png");
            let tray_image = tauri::image::Image::from_path(&tray_icon_path)
                .unwrap_or_else(|_| app.default_window_icon().unwrap().clone());
            let _tray = TrayIconBuilder::with_id("main")
                .icon(tray_image)
                .icon_as_template(true)
                .on_tray_icon_event(move |tray, event| {
                    use tauri::tray::{MouseButton, MouseButtonState};
                    if let tauri::tray::TrayIconEvent::Click {
                        button: MouseButton::Left,
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

            // §9.1 first-run permission check on macOS.
            #[cfg(target_os = "macos")]
            {
                if !crate::tcc::is_accessibility_trusted() {
                    tracing::warn!(
                        "macOS Accessibility permission not granted — opening System Settings pane"
                    );
                    crate::tcc::open_accessibility_settings();
                }
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
        crate::commands::prompts::list_prompts,
        crate::commands::prompts::library_root,
        crate::commands::prompts::save_prompt,
        crate::commands::prompts::create_prompt,
        crate::commands::prompts::delete_prompt,
        crate::commands::prompts::set_prompt_enabled,
        crate::commands::picker::picker_open,
        crate::commands::picker::picker_search,
        crate::commands::picker::picker_select,
        crate::commands::picker::picker_dismiss,
        crate::commands::tray::tray_open,
        crate::commands::tray::tray_quit,
        crate::commands::tray::tray_popup_hide,
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
        for t in &p.triggers {
            entries.push(TriggerEntry {
                canonical: t.to_lowercase(),
                prompt_id: p.id.clone(),
                word_count: t.split_whitespace().count(),
                commit_char: p.commit_char,
            });
        }
    }
    if let Err(e) = ctx.matcher.rebuild_index(entries) {
        tracing::error!("trigger conflict: {}", e);
    }
}

fn apply_window_chrome(app: &tauri::App) {
    for label in ["library", "picker", "settings", "tray-popup"] {
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
        "library" | "settings" => {
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
        // Library / settings — no chrome, no vibrancy. CSS backdrop-filter
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
    let mut builder = builder
        .manage(ctx.state.clone())
        .manage(ctx.prompts.clone())
        .manage(ctx);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        builder = builder.manage(OutsideClickMonitor::shared());
    }
    builder
}
