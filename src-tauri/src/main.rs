// Phase 4: full Tauri integration — hook + matcher + typer + tray + hotkeys.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use prompt_player::hook::spawn_grabbing_hook;
use prompt_player::inject::EnigoInjector;
use prompt_player::matcher::{MatcherState, TriggerEntry};
use prompt_player::picker::{prepare_picker, FocusStore, SearchHit, SearchIndex, RESTORATION_DELAY};
use prompt_player::prompts::library;
use prompt_player::prompts::placeholders::{expand, PlaceholderContext};
use prompt_player::prompts::Prompt;
use prompt_player::state::AppState;
use prompt_player::typer::{play, schedule, ScheduleOptions};
use prompt_player::undo::UndoLog;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::sync::Arc;
use std::thread;
use tauri::{
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};
use std::collections::HashMap;
use std::str::FromStr;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Map: hotkey string (as authored in YAML, e.g. "cmd+shift+1") -> prompt_id.
type PromptHotkeyMap = Arc<parking_lot::RwLock<HashMap<String, String>>>;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "prompt_player=info,info".into()),
        )
        .init();

    // Shared runtime state.
    let app_state = AppState::shared();
    let matcher = MatcherState::shared();
    let undo = Arc::new(UndoLog::new());
    let prompts: Arc<parking_lot::RwLock<Vec<Prompt>>> =
        Arc::new(parking_lot::RwLock::new(Vec::new()));
    let prompt_hotkeys: PromptHotkeyMap = Arc::new(parking_lot::RwLock::new(HashMap::new()));

    // Phase 5: load prompts from the library directory (with hot reload).
    let library_root = library::default_library_root().unwrap_or_else(|| {
        std::env::current_dir().unwrap().join("prompts-examples")
    });
    if let Err(e) = std::fs::create_dir_all(&library_root) {
        tracing::warn!("could not create library dir {:?}: {}", library_root, e);
    }
    {
        let (loaded, errs) = library::load_all(&library_root);
        for e in &errs {
            tracing::warn!("prompt parse: {}", e);
        }
        if loaded.is_empty() {
            // Fall back to the bundled examples for first-run.
            let examples = std::env::current_dir().unwrap().join("prompts-examples");
            if examples != library_root {
                let (l2, _) = library::load_all(&examples);
                *prompts.write() = l2;
            }
        } else {
            *prompts.write() = loaded;
        }
        tracing::info!("loaded {} prompt(s) from {:?}", prompts.read().len(), library_root);
        rebuild_match_index(&prompts.read(), &matcher);
    }

    // Watch for hot reload. We need an AppHandle to re-register hotkeys; defer
    // spawning the thread until Tauri startup so we have one. For now we
    // construct a lazy handle holder that the setup() closure fills.
    let app_handle_holder: Arc<parking_lot::RwLock<Option<tauri::AppHandle>>> =
        Arc::new(parking_lot::RwLock::new(None));
    if let Ok(watcher) = library::watch(&library_root) {
        let prompts2 = prompts.clone();
        let matcher2 = matcher.clone();
        let hotkeys2 = prompt_hotkeys.clone();
        let handle2 = app_handle_holder.clone();
        let root2 = library_root.clone();
        let app_state2 = app_state.clone();
        thread::Builder::new()
            .name("prompt-player-watch".into())
            .spawn(move || loop {
                if library::drain_events(&watcher, std::time::Duration::from_millis(500)) {
                    let (loaded, errs) = library::load_all(&root2);
                    for e in errs {
                        tracing::warn!("hot-reload parse: {}", e);
                    }
                    *prompts2.write() = loaded;
                    rebuild_match_index(&prompts2.read(), &matcher2);
                    if let Some(h) = handle2.read().clone() {
                        rebuild_prompt_hotkeys(&h, &prompts2.read(), &hotkeys2);
                        refresh_tray_popup(&h);
                    }
                    tracing::info!("library hot-reloaded — {} prompt(s)", prompts2.read().len());
                }
            })
            .expect("spawn watch thread");
    }

    // Closures the hook calls back into.
    let on_fire: Arc<dyn Fn(Vec<String>, String) + Send + Sync> = {
        let app_state = app_state.clone();
        let prompts = prompts.clone();
        let undo = undo.clone();
        Arc::new(move |candidate_ids: Vec<String>, typed_form: String| {
            // §4 — capture foreground app, then pick the best candidate by scope+priority+specificity.
            let ctx = prompt_player::scopes::capture_foreground_context();
            let candidate_prompts: Vec<Prompt> = {
                let all = prompts.read();
                candidate_ids
                    .iter()
                    .filter_map(|id| all.iter().find(|p| &p.id == id).cloned())
                    .filter(|p| p.enabled)
                    .collect()
            };
            if candidate_prompts.is_empty() {
                tracing::info!("trigger matched, but no enabled candidate; nothing fires");
                return;
            }
            let Some(picked_id) = prompt_player::scopes::pick_best(&candidate_prompts, &ctx)
            else {
                tracing::info!("no scope match for trigger; nothing fires");
                return;
            };
            let prompt_opt = candidate_prompts
                .into_iter()
                .find(|p| p.id == picked_id);
            let Some(prompt) = prompt_opt else {
                tracing::warn!("on_fire: picked prompt {} not found", picked_id);
                return;
            };
            let cancel = app_state.begin_playback();
            let app_state2 = app_state.clone();
            let undo2 = undo.clone();
            thread::Builder::new()
                .name("prompt-player-typer".into())
                .spawn(move || {
                    // Expand placeholders BEFORE case propagation so `$SELECTION`
                    // resolves to the actual user-typed selection casing.
                    let mut ph_ctx = PlaceholderContext::default();
                    ph_ctx.app_bundle = ctx.bundle_id.clone();
                    ph_ctx.app_name = ctx
                        .executable
                        .as_deref()
                        .and_then(|s| std::path::Path::new(s).file_name().and_then(|f| f.to_str()))
                        .map(|s| s.to_string());
                    ph_ctx.window_title = ctx.window_title.clone();
                    // §6 — expand `${{ TS expr }}` first (lazy evaluation: only evaluated when reached, but for v1 we evaluate eagerly per spec note).
                    let mut expr_ctx = prompt_player::prompts::expressions::ExprContext::default();
                    expr_ctx.app_bundle = ctx.bundle_id.clone();
                    expr_ctx.app_name = ph_ctx.app_name.clone();
                    expr_ctx.window_title = ctx.window_title.clone();
                    let body_after_expr = prompt_player::prompts::expressions::expand_expressions(
                        &prompt.body,
                        &expr_ctx,
                    );
                    let expanded = expand(&body_after_expr, &ph_ctx);
                    // §5.6 — apply filter chain.
                    let filtered = prompt_player::filters::apply_chain(&expanded.text, &prompt.filters);
                    let body = prompt_player::matcher::propagate_case(&typed_form, &filtered);
                    let profile = prompt.effective_profile();
                    // §9.3 — RDP detection.
                    let rdp = prompt_player::rdp::RdpRegistry::new();
                    let rdp_mode = rdp.detect(&ctx);
                    let opts = ScheduleOptions {
                        rdp_mode: rdp_mode == prompt_player::rdp::RdpMode::HostSide,
                        include_pre_typing_pause: true,
                    };
                    if rdp_mode == prompt_player::rdp::RdpMode::HostSide {
                        tracing::info!("rdp host-side mode active for {:?}", ctx.bundle_id.as_deref().or(ctx.executable.as_deref()));
                    }
                    let mut rng = ChaCha8Rng::from_entropy();
                    let s = schedule(&body, &profile, &opts, &mut rng);
                    let body_chars = s
                        .iter()
                        .filter(|k| matches!(k.key, prompt_player::typer::Key::Char(_)))
                        .count()
                        - s.iter()
                            .filter(|k| matches!(k.key, prompt_player::typer::Key::Backspace))
                            .count();
                    match EnigoInjector::new() {
                        Ok(mut inj) => {
                            let completed = play(&s, &mut inj, cancel);
                            if completed {
                                undo2.record(typed_form, body_chars);
                            }
                        }
                        Err(e) => tracing::error!("enigo init failed: {}", e),
                    }
                    app_state2.end_playback();
                })
                .expect("spawn typer thread");
        })
    };

    let on_undo: Arc<dyn Fn() + Send + Sync> = {
        let undo = undo.clone();
        let app_state = app_state.clone();
        Arc::new(move || {
            let Some(entry) = undo.take_recent(std::time::Instant::now()) else {
                return;
            };
            let cancel = app_state.begin_playback();
            let app_state2 = app_state.clone();
            let trigger = entry.trigger_form.clone();
            let body_chars = entry.body_chars_typed;
            thread::Builder::new()
                .name("prompt-player-undo".into())
                .spawn(move || {
                    let mut inj = match EnigoInjector::new() {
                        Ok(i) => i,
                        Err(e) => {
                            tracing::error!("undo enigo init failed: {}", e);
                            app_state2.end_playback();
                            return;
                        }
                    };
                    use prompt_player::typer::Injector;
                    for _ in 0..body_chars {
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            inj.release_all_modifiers();
                            app_state2.end_playback();
                            return;
                        }
                        inj.press_backspace();
                        thread::sleep(std::time::Duration::from_millis(15));
                    }
                    for c in trigger.chars() {
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            inj.release_all_modifiers();
                            app_state2.end_playback();
                            return;
                        }
                        inj.type_char(c);
                        thread::sleep(std::time::Duration::from_millis(20));
                    }
                    app_state2.end_playback();
                })
                .expect("spawn undo thread");
        })
    };

    // Spawn the hook thread BEFORE Tauri startup so it owns its own thread lifecycle.
    let _hook = spawn_grabbing_hook(
        matcher.clone(),
        undo.clone(),
        app_state.clone(),
        on_fire.clone(),
        on_undo.clone(),
    );

    let app_state_for_setup = app_state.clone();
    let prompt_hotkeys_for_setup = prompt_hotkeys.clone();
    let on_fire_for_setup = on_fire.clone();
    let app_handle_holder_for_setup = app_handle_holder.clone();
    let prompts_for_setup = prompts.clone();
    prompt_player::telemetry::init();

    // Aptabase plugin requires a Tokio runtime that Tauri 2.x doesn't auto-provide;
    // re-add once telemetry::send() actually has call sites and we wire a runtime.
    // For now `telemetry::init()` logs the key; events are no-ops.
    // global-shortcut plugin is registered later in register_shortcuts() with its handler.
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state.clone())
        .manage(matcher.clone())
        .manage(prompts.clone())
        .manage(undo.clone())
        .manage(FocusStore::shared())
        .manage(parking_lot::Mutex::new(SearchIndex::new()))
        .manage({
            #[cfg(target_os = "macos")]
            { OutsideClickMonitor::shared() }
            #[cfg(not(target_os = "macos"))]
            { () }
        })
        .invoke_handler(tauri::generate_handler![
            ipc_get_armed,
            ipc_toggle_armed,
            ipc_kill,
            ipc_list_prompts,
            ipc_save_prompt,
            ipc_create_prompt,
            ipc_delete_prompt,
            ipc_library_root,
            ipc_picker_open,
            ipc_picker_search,
            ipc_picker_select,
            ipc_picker_dismiss,
            ipc_set_prompt_enabled,
            ipc_tray_open,
            ipc_tray_quit,
            ipc_tray_popup_hide,
        ])
        .setup(move |app| {
            // Load tray icon from disk so dev-loop edits are picked up without a rebuild.
            let tray_icon_path = std::env::current_dir()
                .unwrap()
                .join("src-tauri/icons/tray-icon.png");
            let tray_image = tauri::image::Image::from_path(&tray_icon_path)
                .unwrap_or_else(|_| app.default_window_icon().unwrap().clone());
            // We DON'T attach an NSMenu to the tray — NSMenu auto-dismisses on
            // every item click, which conflicts with the WiFi-style stay-open
            // popover the user expects. Instead we listen for click events and
            // show our custom borderless `tray-popup` window positioned under
            // the tray icon.
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
                        toggle_tray_popup(app, rect);
                    }
                })
                .build(app)?;

            // Global shortcuts.
            register_shortcuts(
                app,
                app_state_for_setup.clone(),
                prompt_hotkeys_for_setup.clone(),
                on_fire_for_setup.clone(),
            )?;

            // Make AppHandle available to the hot-reload watch thread so it can
            // re-register prompt hotkeys when the library changes.
            *app_handle_holder_for_setup.write() = Some(app.handle().clone());

            // Initial registration of prompt-defined hotkeys.
            rebuild_prompt_hotkeys(
                &app.handle().clone(),
                &prompts_for_setup.read(),
                &prompt_hotkeys_for_setup,
            );

            // Intercept window close → hide instead of destroy. Otherwise the
            // user's first X-click destroys the window and re-opening from the
            // tray menu silently fails.
            for label in ["library", "picker", "settings", "tray-popup"] {
                if let Some(w) = app.get_webview_window(label) {
                    // Native NSVisualEffectView only on the picker (borderless).
                    // For library/settings, NSVisualEffectView intercepts mouse
                    // events on macOS Sonoma+ and breaks `data-tauri-drag-region`.
                    // CSS `backdrop-filter` on the inner panels still gives glass.
                    #[cfg(target_os = "macos")]
                    if label == "picker" {
                        use window_vibrancy::{
                            apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState,
                        };
                        let _ = apply_vibrancy(
                            &w,
                            NSVisualEffectMaterial::HudWindow,
                            Some(NSVisualEffectState::Active),
                            Some(12.0),
                        );
                        configure_picker_window(&w);
                    }
                    #[cfg(target_os = "macos")]
                    if label == "tray-popup" {
                        use window_vibrancy::{
                            apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState,
                        };
                        // `HudWindow` produces visibly translucent vibrancy
                        // (closer to the colors that native NSMenus / Control
                        // Center popovers show through over a desktop). 10px
                        // corner radius matches Tahoe's NSMenu rounding.
                        let _ = apply_vibrancy(
                            &w,
                            NSVisualEffectMaterial::HudWindow,
                            Some(NSVisualEffectState::Active),
                            Some(10.0),
                        );
                        // Convert the underlying NSWindow into an NSPanel so
                        // it can float above fullscreen apps. Done AFTER
                        // vibrancy so window-vibrancy's setup is not
                        // overwritten.
                        configure_popover_window(&w);
                    }
                    // Library + settings: make them space-neutral so when
                    // we activate the app to summon the picker, macOS
                    // doesn't switch the user's Space to wherever these
                    // windows happen to live.
                    #[cfg(target_os = "macos")]
                    if label == "library" || label == "settings" {
                        make_window_space_neutral(&w);
                    }
                    let w_clone = w.clone();
                    let label_owned = label.to_string();
                    let app_handle_for_event = app.handle().clone();
                    w.on_window_event(move |e| match e {
                        tauri::WindowEvent::CloseRequested { api, .. } => {
                            api.prevent_close();
                            let _ = w_clone.hide();
                        }
                        // The tray popup is a click-out-to-dismiss popover —
                        // hide it as soon as it loses focus, mirroring native
                        // NSMenu behavior.
                        tauri::WindowEvent::Focused(false) if label_owned == "tray-popup" => {
                            let _ = w_clone.hide();
                            #[cfg(target_os = "macos")]
                            if let Some(monitor) = app_handle_for_event
                                .try_state::<Arc<OutsideClickMonitor>>()
                            {
                                remove_outside_click_monitor(monitor.inner());
                            }
                        }
                        // Picker — Spotlight-style. When the user clicks
                        // Outside-click / focus-loss dismiss. Hide and
                        // restore focus to the previously-active app
                        // (Spotlight-style).
                        tauri::WindowEvent::Focused(false) if label_owned == "picker" => {
                            let _ = w_clone.hide();
                            if let Some(focus) =
                                app_handle_for_event.try_state::<Arc<FocusStore>>()
                            {
                                let _ = focus.restore();
                            }
                        }
                        _ => {}
                    });
                }
            }

            // Tray-only / menu-bar utility behavior: no Dock icon, no
            // Cmd+Tab entry, AND — critically — `NSApp.activate(ignoringOtherApps:)`
            // does NOT yank the user out of fullscreen-app Spaces when the
            // policy is `.accessory`. This is how Raycast / Alfred / native
            // popovers stay in the user's current Space when summoned.
            #[cfg(target_os = "macos")]
            {
                use cocoa::base::id;
                use objc::{class, msg_send, sel, sel_impl};
                unsafe {
                    let nsapp: id = msg_send![class!(NSApplication), sharedApplication];
                    // NSApplicationActivationPolicyAccessory = 1.
                    let _: () = msg_send![nsapp, setActivationPolicy: 1i64];
                }
            }

            // §9.1 first-run permission check on macOS.
            #[cfg(target_os = "macos")]
            {
                if !prompt_player::tcc::is_accessibility_trusted() {
                    tracing::warn!(
                        "macOS Accessibility permission not granted — opening System Settings pane"
                    );
                    prompt_player::tcc::open_accessibility_settings();
                }
            }

            // §9.1 — periodically poll macOS Secure Input state and log when it changes.
            // Surfaces the "Terminal has Secure Keyboard Entry on" gotcha that's
            // otherwise silent (the tap simply never receives events).
            #[cfg(target_os = "macos")]
            {
                std::thread::spawn(|| {
                    let mut last = false;
                    loop {
                        let now_active = prompt_player::secure_input::is_active();
                        if now_active != last {
                            if now_active {
                                tracing::warn!(
                                    "macOS Secure Input is ACTIVE — keyboard hook is BLOCKED. \
                                     Likely cause: Terminal's 'Secure Keyboard Entry' is on, or \
                                     a password field is focused."
                                );
                            } else {
                                tracing::info!("macOS Secure Input cleared; keyboard hook active again.");
                            }
                            last = now_active;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                });
            }

            tracing::info!("Prompt Player started — disarmed (per §10.1)");
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Keep the app alive when all windows close — we're a tray app.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}

fn show_window(app: &tauri::AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        // Bring the macOS process to the foreground BEFORE showing the window —
        // tray-only apps without a Dock icon otherwise can't grab focus and the
        // newly-shown window gets back-grounded immediately ("disappears").
        #[cfg(target_os = "macos")]
        activate_macos_app();
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Show a borderless NSPanel popover (tray popup or command palette) WITHOUT
/// activating the Prompt Player app. Tauri's `WebviewWindow::set_focus` calls
/// `[NSApp activateIgnoringOtherApps:YES]` which steals focus from the
/// foreground app — the opposite of what a stealth/menu-bar utility wants.
/// We use `makeKeyAndOrderFront:` instead, which on an NSPanel marked
/// nonactivating both orders the panel front and triggers the first-
/// responder handoff (so the WKWebView can accept keystrokes) without
/// promoting the Prompt Player app to the foreground.
#[cfg(target_os = "macos")]
fn order_panel_front_no_activate(window: &tauri::WebviewWindow) {
    use cocoa::base::{id, nil};
    use objc::{msg_send, sel, sel_impl};
    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    unsafe {
        let ns_window: id = ns_window_ptr as id;
        let _: () = msg_send![ns_window, makeKeyAndOrderFront: nil];
    }
}
#[cfg(not(target_os = "macos"))]
fn order_panel_front_no_activate(window: &tauri::WebviewWindow) {
    let _ = window.set_focus();
}

/// macOS-only: shared cell that holds the NSEvent global mouse monitor while
/// the popover is visible. We install it on show and remove it on hide so any
/// click outside the popover (anywhere in the system) dismisses it the way a
/// native popover does.
#[cfg(target_os = "macos")]
struct OutsideClickMonitor {
    outside: parking_lot::Mutex<Option<usize>>,
    mouse_track: parking_lot::Mutex<Option<usize>>,
}
#[cfg(target_os = "macos")]
impl OutsideClickMonitor {
    fn shared() -> Arc<Self> {
        Arc::new(Self {
            outside: parking_lot::Mutex::new(None),
            mouse_track: parking_lot::Mutex::new(None),
        })
    }
}

#[cfg(target_os = "macos")]
fn install_outside_click_monitor(app: &AppHandle, monitor_state: &Arc<OutsideClickMonitor>) {
    use block::ConcreteBlock;
    use cocoa::base::{id, nil};
    use objc::{class, msg_send, sel, sel_impl};

    // Already installed? Don't double up.
    if monitor_state.outside.lock().is_some() {
        return;
    }
    // NSEventMaskLeftMouseDown (1<<1) | NSEventMaskRightMouseDown (1<<3) |
    // NSEventMaskOtherMouseDown (1<<25).
    let mask: u64 = (1u64 << 1) | (1u64 << 3) | (1u64 << 25);
    let app2 = app.clone();
    let block = ConcreteBlock::new(move |_event: id| {
        if let Some(w) = app2.get_webview_window("tray-popup") {
            let _ = w.hide();
        }
    });
    let block = block.copy();
    unsafe {
        let nsevent_class = class!(NSEvent);
        let monitor: id = msg_send![
            nsevent_class,
            addGlobalMonitorForEventsMatchingMask: mask
            handler: &*block
        ];
        if monitor != nil {
            *monitor_state.outside.lock() = Some(monitor as usize);
        }
    }

    // ALSO install a LOCAL monitor for mouseMoved so we can feed cursor
    // positions to the popover webview. WKWebView inside a non-activating
    // NSPanel doesn't dispatch mouseMoved events to JS, leaving CSS :hover
    // and JS hover state frozen on whatever the last received event was. By
    // intercepting mouseMoved at the NSEvent layer and emitting them as
    // Tauri events, we can drive hover from the OS event stream regardless
    // of what the WKWebView decides to forward.
    if monitor_state.mouse_track.lock().is_some() {
        return;
    }
    // NSEventMaskMouseMoved = 1 << 5.
    let move_mask: u64 = 1u64 << 5;
    let app3 = app.clone();
    let move_block = ConcreteBlock::new(move |event: id| -> id {
        // We must return the event unchanged so normal dispatch continues.
        unsafe {
            if let Some(w) = app3.get_webview_window("tray-popup") {
                if let Ok(ns_window_ptr) = w.ns_window() {
                    let ns_window: id = ns_window_ptr as id;
                    // locationInWindow is in the panel's window coordinate
                    // space when the event window matches; otherwise we
                    // convert from screen.
                    let ev_window: id = msg_send![event, window];
                    let loc: cocoa::foundation::NSPoint = if ev_window == ns_window {
                        msg_send![event, locationInWindow]
                    } else {
                        let screen_loc: cocoa::foundation::NSPoint = msg_send![event, locationInWindow];
                        // Convert to screen, then to our window's coords.
                        let global: cocoa::foundation::NSPoint = if ev_window != nil {
                            let pt: cocoa::foundation::NSPoint = msg_send![ev_window, convertPointToScreen: screen_loc];
                            pt
                        } else {
                            // No source window — treat as global already.
                            screen_loc
                        };
                        let p: cocoa::foundation::NSPoint = msg_send![ns_window, convertPointFromScreen: global];
                        p
                    };
                    // Convert AppKit (origin bottom-left) to web (origin top-left).
                    let frame: cocoa::foundation::NSRect = msg_send![ns_window, frame];
                    let css_x = loc.x;
                    let css_y = frame.size.height - loc.y;
                    let _ = w.emit("tray-popup-mousemove", (css_x, css_y));
                }
            }
        }
        event
    });
    let move_block = move_block.copy();
    unsafe {
        let nsevent_class = class!(NSEvent);
        let monitor: id = msg_send![
            nsevent_class,
            addLocalMonitorForEventsMatchingMask: move_mask
            handler: &*move_block
        ];
        if monitor != nil {
            *monitor_state.mouse_track.lock() = Some(monitor as usize);
        }
    }
}

#[cfg(target_os = "macos")]
fn remove_outside_click_monitor(monitor_state: &Arc<OutsideClickMonitor>) {
    use cocoa::base::id;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        if let Some(ptr) = monitor_state.outside.lock().take() {
            let nsevent_class = class!(NSEvent);
            let _: () = msg_send![nsevent_class, removeMonitor: ptr as id];
        }
        if let Some(ptr) = monitor_state.mouse_track.lock().take() {
            let nsevent_class = class!(NSEvent);
            let _: () = msg_send![nsevent_class, removeMonitor: ptr as id];
        }
    }
}

/// Spotlight-style picker config — the canonical recipe used by
/// `tauri-nspanel` / `alt-tab-macos` / Hammerspoon's Chooser:
///
/// 1. Subclass NSPanel with a runtime class that overrides
///    `-canBecomeKeyWindow` to return YES. NSPanel with `nonActivatingPanel`
///    style mask defaults to NO, which is why a plain non-activating panel
///    silently swallows all keystrokes — they fall through to the
///    underlying app.
/// 2. `styleMask |= NonactivatingPanel` so the panel can be key without
///    triggering app activation (which would switch Spaces).
/// 3. `collectionBehavior = CanJoinAllSpaces | FullScreenAuxiliary` so the
///    panel surfaces on whatever Space is current (incl. fullscreen).
/// 4. `level = NSPopUpMenuWindowLevel`.
/// 5. Show via `makeKeyAndOrderFront(nil)`. **Never** call
///    `NSApp.activate(ignoringOtherApps:)` — that's what was switching
///    Spaces on every previous attempt.
#[cfg(target_os = "macos")]
fn configure_picker_window(window: &tauri::WebviewWindow) {
    use cocoa::base::id;
    use objc::runtime::Class;
    use objc::{class, msg_send, sel, sel_impl};
    extern "C" {
        fn object_setClass(obj: id, cls: *const Class) -> *const Class;
    }
    let Ok(ns_window_ptr) = window.ns_window() else { return };
    unsafe {
        let ns_window: id = ns_window_ptr as id;

        let panel_class = register_picker_panel_class();
        object_setClass(ns_window, panel_class);

        let current_mask: u64 = msg_send![ns_window, styleMask];
        let _: () = msg_send![ns_window, setStyleMask: current_mask | (1u64 << 7)];

        let collection: u64 = (1u64 << 0) | (1u64 << 8);
        let _: () = msg_send![ns_window, setCollectionBehavior: collection];

        let _: () = msg_send![ns_window, setLevel: 101 as std::os::raw::c_long];
        let _: () = msg_send![ns_window, setBecomesKeyOnlyIfNeeded: 0u8];
        let _: () = msg_send![ns_window, setFloatingPanel: 1u8];
        let _: () = msg_send![ns_window, setHidesOnDeactivate: 0u8];
        let _: () = msg_send![ns_window, setHasShadow: 1u8];
    }
}
#[cfg(not(target_os = "macos"))]
fn configure_picker_window(_window: &tauri::WebviewWindow) {}

/// Register an Obj-C class `PromptPlayerKeyPanel` (NSPanel subclass) that
/// overrides `-canBecomeKeyWindow` and `-canBecomeMainWindow` to return YES.
/// This is the trick that lets a `nonActivatingPanel` actually receive
/// keystrokes — without it the panel can never be key, so keys flow to the
/// underlying app no matter what other flags are set.
#[cfg(target_os = "macos")]
fn register_picker_panel_class() -> *const objc::runtime::Class {
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel, BOOL, YES};
    use objc::{class, sel, sel_impl};
    use std::sync::Once;

    extern "C" fn can_become_key(_this: &Object, _cmd: Sel) -> BOOL {
        YES
    }
    extern "C" fn can_become_main(_this: &Object, _cmd: Sel) -> BOOL {
        YES
    }

    static mut CLASS: *const Class = std::ptr::null();
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let superclass = class!(NSPanel);
        let mut decl = ClassDecl::new("PromptPlayerKeyPanel", superclass)
            .expect("register PromptPlayerKeyPanel");
        decl.add_method(
            sel!(canBecomeKeyWindow),
            can_become_key as extern "C" fn(&Object, Sel) -> BOOL,
        );
        decl.add_method(
            sel!(canBecomeMainWindow),
            can_become_main as extern "C" fn(&Object, Sel) -> BOOL,
        );
        CLASS = decl.register();
    });
    unsafe { CLASS }
}

/// Apply `CanJoinAllSpaces | FullScreenAuxiliary` collection behavior to a
/// non-popover window (library, settings). Ensures that when our app is
/// activated to summon the picker, macOS doesn't switch the user back to
/// whatever Space these windows happen to live on.
#[cfg(target_os = "macos")]
fn make_window_space_neutral(window: &tauri::WebviewWindow) {
    use cocoa::base::id;
    use objc::{msg_send, sel, sel_impl};
    let Ok(ns_window_ptr) = window.ns_window() else { return };
    unsafe {
        let ns_window: id = ns_window_ptr as id;
        let collection: u64 = (1u64 << 0) | (1u64 << 8);
        let _: () = msg_send![ns_window, setCollectionBehavior: collection];
    }
}
#[cfg(not(target_os = "macos"))]
fn make_window_space_neutral(_window: &tauri::WebviewWindow) {}

/// Reposition the picker centered horizontally on the screen that contains
/// the cursor, ~30% from the top (Spotlight placement). Does NOT show or
/// activate — call `show_window("picker")` after this.
#[cfg(target_os = "macos")]
fn position_picker_on_cursor_screen(app: &AppHandle) {
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSPoint, NSRect};
    use objc::{class, msg_send, sel, sel_impl};

    let Some(window) = app.get_webview_window("picker") else { return };
    let Ok(ns_window_ptr) = window.ns_window() else { return };
    unsafe {
        let ns_window: id = ns_window_ptr as id;
        let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let screens: id = msg_send![class!(NSScreen), screens];
        if screens == nil { return; }
        let count: usize = msg_send![screens, count];
        let mut chosen_visible: Option<NSRect> = None;
        for i in 0..count {
            let screen: id = msg_send![screens, objectAtIndex: i];
            let frame: NSRect = msg_send![screen, frame];
            if cursor.x >= frame.origin.x
                && cursor.x <= frame.origin.x + frame.size.width
                && cursor.y >= frame.origin.y
                && cursor.y <= frame.origin.y + frame.size.height
            {
                let vf: NSRect = msg_send![screen, visibleFrame];
                chosen_visible = Some(vf);
                break;
            }
        }
        let Some(vf) = chosen_visible else { return };
        let win_frame: NSRect = msg_send![ns_window, frame];
        let x = vf.origin.x + (vf.size.width - win_frame.size.width) / 2.0;
        let y_top_pad = vf.size.height * 0.30;
        let y = vf.origin.y + vf.size.height - y_top_pad - win_frame.size.height;
        let origin = NSPoint { x, y };
        let _: () = msg_send![ns_window, setFrameOrigin: origin];
    }
}
#[cfg(not(target_os = "macos"))]
fn position_picker_on_cursor_screen(_app: &AppHandle) {}

/// Show the picker positioned on the screen the user is currently on
/// (cursor's screen, falling back to the primary). Must be called from the
/// main thread — AppKit calls (NSEvent.mouseLocation, NSScreen.screens,
/// setFrameOrigin:) are not thread-safe and return stale data off-thread.
///
/// Sequence (matches the Multi.app / Raycast recipe):
/// 1. Read NSEvent.mouseLocation, find which NSScreen contains it.
/// 2. Compute the target NSPoint in AppKit bottom-left coords.
/// 3. Use the alpha-0 trick: setAlphaValue:0 → makeKeyAndOrderFront → set
///    the frame on that screen → setAlphaValue:1. setFrameOrigin alone on
///    a hidden window doesn't bind the window to the new NSScreen, which is
///    why every previous attempt landed on the primary.
fn show_picker_on_active_screen(app: &AppHandle) {
    let Some(window) = app.get_webview_window("picker") else { return };
    let _ = prepare_picker(app, true);

    #[cfg(target_os = "macos")]
    {
        use cocoa::base::{id, nil};
        use cocoa::foundation::{NSPoint, NSRect};
        use objc::{class, msg_send, sel, sel_impl};

        // Force any other visible windows of our app to orderOut so when
        // we activate, macOS has no other window to "bring forward" — it
        // can only bring the picker forward, which has CanJoinAllSpaces and
        // therefore lands on the user's current Space.
        for other in ["library", "settings", "tray-popup"] {
            if let Some(w) = app.get_webview_window(other) {
                let _ = w.hide();
            }
        }

        let Ok(ns_window_ptr) = window.ns_window() else { return };
        unsafe {
            let ns_window: id = ns_window_ptr as id;

            // Find the screen the cursor is currently on. We DO NOT activate
            // the app — doing so would pull a user inside a fullscreen-app
            // Space back to our home Space.
            let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
            let screens: id = msg_send![class!(NSScreen), screens];
            let mut chosen_visible: Option<NSRect> = None;
            let mut chosen_idx: i64 = -1;
            tracing::info!("picker show: cursor=({:.1}, {:.1})", cursor.x, cursor.y);
            if screens != nil {
                let count: usize = msg_send![screens, count];
                for i in 0..count {
                    let screen: id = msg_send![screens, objectAtIndex: i];
                    let frame: NSRect = msg_send![screen, frame];
                    tracing::info!(
                        "  screen[{}] frame=(x={:.1}, y={:.1}, w={:.1}, h={:.1})",
                        i, frame.origin.x, frame.origin.y, frame.size.width, frame.size.height
                    );
                    if cursor.x >= frame.origin.x
                        && cursor.x <= frame.origin.x + frame.size.width
                        && cursor.y >= frame.origin.y
                        && cursor.y <= frame.origin.y + frame.size.height
                    {
                        let vf: NSRect = msg_send![screen, visibleFrame];
                        chosen_visible = Some(vf);
                        chosen_idx = i as i64;
                        break;
                    }
                }
            }
            tracing::info!("  → chosen screen index = {}", chosen_idx);

            let win_frame: NSRect = msg_send![ns_window, frame];
            tracing::info!(
                "  win_frame: x={:.1} y={:.1} w={:.1} h={:.1}",
                win_frame.origin.x, win_frame.origin.y, win_frame.size.width, win_frame.size.height
            );
            let target_origin = if let Some(vf) = chosen_visible {
                let x = vf.origin.x + (vf.size.width - win_frame.size.width) / 2.0;
                let y_top_pad = vf.size.height * 0.30;
                let y = vf.origin.y + vf.size.height - y_top_pad - win_frame.size.height;
                tracing::info!("  target origin: ({:.1}, {:.1})", x, y);
                NSPoint { x, y }
            } else {
                tracing::warn!("  no screen matched cursor — keeping current origin");
                win_frame.origin
            };

            // Verbatim tauri-nspanel `show_and_make_key` recipe:
            //   1. makeFirstResponder(contentView)  — BEFORE ordering, so
            //      the WKWebView is responder when the panel becomes key
            //   2. orderFrontRegardless              — show without a key
            //      transition (avoids triggering app activation)
            //   3. makeKeyWindow                      — explicit key set
            //   4. setFrameOrigin                     — bind to target
            //      screen now that the panel is on screen
            // This is what the canBecomeKeyWindow=YES override on our
            // subclass (PromptPlayerKeyPanel) enables: a non-activating
            // panel that actually receives keystrokes.
            // Same pattern as `show_window` (which works reliably from the
            // tray menu): activate app → Tauri show + set_focus. With
            // `.accessory` activation policy + space-neutral library/
            // settings windows, this does NOT switch the user's Space.
            // The all-msg_send sequence we tried before raced the Tauri
            // show() runloop posting and made every other summon fail.
            let _: () = msg_send![ns_window, setAlphaValue: 0.0f64];
            activate_macos_app();
            let _ = window.show();
            let _ = window.set_focus();
            let _: () = msg_send![ns_window, setFrameOrigin: target_origin];
            // Force first-responder rebuild on each show: clear to nil, then
            // re-set to contentView. The cached `contentView` firstResponder
            // pointer survives hide/show cycles, making a single
            // makeFirstResponder a no-op the second time.
            let content_view: id = msg_send![ns_window, contentView];
            let _: bool = msg_send![ns_window, makeFirstResponder: nil];
            if content_view != nil {
                let _: bool = msg_send![ns_window, makeFirstResponder: content_view];
            }
            let _: () = msg_send![ns_window, setAlphaValue: 1.0f64];
        }
        let _ = window.emit("picker-shown", ());
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("picker-shown", ());
    }
}

/// Configure the tray popover window to behave like a native menu-bar popover:
/// - Converts the underlying NSWindow into NSPanel (isa-swizzle) so the
///   `NonactivatingPanel` style mask is actually honored. This is the
///   technique the `tauri-nspanel` community plugin uses; without it a
///   regular NSWindow cannot float over fullscreen apps no matter what
///   collection behavior or level is set.
/// - Joins the active space (including fullscreen-app spaces).
/// - Bumps level to NSPopUpMenuWindowLevel.
#[cfg(target_os = "macos")]
fn configure_popover_window(window: &tauri::WebviewWindow) {
    use cocoa::base::id;
    use objc::runtime::Class;
    use objc::{class, msg_send, sel, sel_impl};
    extern "C" {
        fn object_setClass(obj: id, cls: *const Class) -> *const Class;
    }
    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    unsafe {
        let ns_window: id = ns_window_ptr as id;

        // Re-class the window's Obj-C isa pointer to NSPanel. After this the
        // same object responds to NSPanel-only behaviors and the
        // NonactivatingPanel style flag becomes effective. The transformation
        // is sticky (no need to repeat per show).
        let nspanel: *const Class = class!(NSPanel);
        object_setClass(ns_window, nspanel);

        // OR in NSWindowStyleMaskNonactivatingPanel (1 << 7). Now that the
        // window is an NSPanel, this flag actually takes effect: the window
        // can be key without activating the app, which is exactly the
        // semantics of a native menu-bar popover.
        let current_mask: u64 = msg_send![ns_window, styleMask];
        let new_mask: u64 = current_mask | (1u64 << 7);
        let _: () = msg_send![ns_window, setStyleMask: new_mask];

        // NSWindowCollectionBehaviorMoveToActiveSpace (1 << 1) |
        // NSWindowCollectionBehaviorFullScreenAuxiliary (1 << 8) — the two
        // flags that together make a panel surface on a fullscreen app's
        // dedicated space.
        let collection: u64 = (1u64 << 1) | (1u64 << 8);
        let _: () = msg_send![ns_window, setCollectionBehavior: collection];

        // NSPopUpMenuWindowLevel = 101.
        let _: () = msg_send![ns_window, setLevel: 101 as std::os::raw::c_long];

        // NSPanel-only: setBecomesKeyOnlyIfNeeded:NO so the panel takes key
        // when shown and Escape reaches the webview's keydown listener.
        // Outside-click dismissal is handled by an NSEvent global monitor
        // installed in install_outside_click_monitor.
        let _: () = msg_send![ns_window, setBecomesKeyOnlyIfNeeded: 0u8];
        let _: () = msg_send![ns_window, setFloatingPanel: 1u8];

        let _: () = msg_send![ns_window, setHidesOnDeactivate: 0u8];
        let _: () = msg_send![ns_window, setExcludedFromWindowsMenu: 1u8];
        let _: () = msg_send![ns_window, setMovableByWindowBackground: 0u8];
        let _: () = msg_send![ns_window, setHasShadow: 1u8];
        // Without this, a non-activating panel does not forward mouse-moved
        // events to the WKWebView, so CSS :hover never fires on hover-only
        // (no click) interactions.
        let _: () = msg_send![ns_window, setAcceptsMouseMovedEvents: 1u8];
        let _: () = msg_send![ns_window, setIgnoresMouseEvents: 0u8];

        // Critical for hover/mouseenter inside a non-activating panel: the
        // panel's contentView must accept first-mouse events so mouse-tracking
        // engages before the panel becomes the app's key window. Without
        // this, WKWebView drops mouse-enter/move until the user has clicked
        // once. Walk the contentView tree and forcibly enable.
        let content_view: id = msg_send![ns_window, contentView];
        if content_view != cocoa::base::nil {
            // Re-evaluate tracking areas on display so the WKWebView sets up
            // its own NSTrackingArea covering the new bounds.
            let _: () = msg_send![content_view, setNeedsDisplay: 1u8];
        }
    }
}

#[cfg(target_os = "macos")]
fn activate_macos_app() {
    use cocoa::base::{id, YES};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: YES];
    }
}

/// Normalize a user-authored hotkey ("cmd+shift+1", "Cmd+Shift+1", "⌘⇧1") into
/// the canonical Tauri parser form ("CmdOrCtrl+Shift+Digit1").
fn normalize_hotkey(input: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for raw in input.split(['+', '-']) {
        let s = raw.trim().to_lowercase();
        let mapped = match s.as_str() {
            "cmd" | "command" | "⌘" | "meta" | "super" | "win" | "windows" => "CmdOrCtrl".into(),
            "ctrl" | "control" | "⌃" => "Control".into(),
            "shift" | "⇧" => "Shift".into(),
            "alt" | "option" | "opt" | "⌥" => "Alt".into(),
            "0" => "Digit0".into(),
            "1" => "Digit1".into(),
            "2" => "Digit2".into(),
            "3" => "Digit3".into(),
            "4" => "Digit4".into(),
            "5" => "Digit5".into(),
            "6" => "Digit6".into(),
            "7" => "Digit7".into(),
            "8" => "Digit8".into(),
            "9" => "Digit9".into(),
            "esc" | "escape" => "Escape".into(),
            "enter" | "return" => "Enter".into(),
            "space" | "spacebar" => "Space".into(),
            "tab" => "Tab".into(),
            "backspace" => "Backspace".into(),
            other if other.len() == 1 && other.chars().next().unwrap().is_ascii_alphabetic() => {
                format!("Key{}", other.to_uppercase())
            }
            other => {
                // Already canonical (Comma, Period, F1-F12, etc.)
                let mut chars = other.chars();
                match chars.next() {
                    Some(c) => format!("{}{}", c.to_uppercase().collect::<String>(), chars.as_str()),
                    None => other.into(),
                }
            }
        };
        parts.push(mapped);
    }
    parts.join("+")
}

fn rebuild_prompt_hotkeys(
    app: &tauri::AppHandle,
    prompts: &[Prompt],
    hotkeys: &PromptHotkeyMap,
) {
    let gs = app.global_shortcut();
    // Unregister previously registered prompt hotkeys.
    {
        let old = hotkeys.read();
        for hk_str in old.keys() {
            if let Ok(s) = Shortcut::from_str(&normalize_hotkey(hk_str)) {
                let _ = gs.unregister(s);
            }
        }
    }
    let mut new_map: HashMap<String, String> = HashMap::new();
    for p in prompts {
        if !p.enabled {
            continue;
        }
        let Some(hk) = &p.hotkey else { continue };
        if hk.trim().is_empty() {
            continue;
        }
        let normalized = normalize_hotkey(hk);
        match Shortcut::from_str(&normalized) {
            Ok(s) => match gs.register(s) {
                Ok(()) => {
                    tracing::info!("registered hotkey {} → {}", hk, p.id);
                    new_map.insert(hk.clone(), p.id.clone());
                }
                Err(e) => tracing::warn!("hotkey {} register failed: {}", hk, e),
            },
            Err(e) => tracing::warn!("hotkey {} unparseable (normalized: {}): {}", hk, normalized, e),
        }
    }
    *hotkeys.write() = new_map;
}

/// Build the tray menu reflecting current armed state and the first up-to-5
/// Persist a prompt's `enabled` flag to disk. The in-memory state is updated
/// in-place so the next IPC list call (and any in-flight popup render) sees
/// the new value immediately; hot-reload will then re-read it idempotently.
fn set_prompt_enabled_inner(
    prompts: &Arc<parking_lot::RwLock<Vec<Prompt>>>,
    prompt_id: &str,
    enabled: bool,
) -> Result<(), String> {
    let snapshot = {
        let mut all = prompts.write();
        let p = all
            .iter_mut()
            .find(|p| p.id == prompt_id)
            .ok_or_else(|| format!("prompt {} not found", prompt_id))?;
        p.enabled = enabled;
        p.clone()
    };
    let path = snapshot
        .source_path
        .clone()
        .ok_or_else(|| format!("prompt {} has no source path", prompt_id))?;
    let body = prompt_player::prompts::parser::serialize(&snapshot)
        .map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("write {path:?}: {e}"))?;
    tracing::info!("prompt {} → enabled={}", snapshot.id, snapshot.enabled);
    Ok(())
}

/// Show or hide the tray popup window in response to a left-click on the
/// tray icon. Position the window immediately under the tray icon's rect so
/// it visually anchors to the menu bar, mirroring native NSMenu placement.
fn toggle_tray_popup(app: &AppHandle, rect: tauri::Rect) {
    let Some(window) = app.get_webview_window("tray-popup") else {
        return;
    };
    let already_visible = window.is_visible().unwrap_or(false);
    if already_visible {
        let _ = window.hide();
        #[cfg(target_os = "macos")]
        if let Some(monitor) = app.try_state::<Arc<OutsideClickMonitor>>() {
            remove_outside_click_monitor(monitor.inner());
        }
        return;
    }
    // Position the window so its top edge sits 4px below the tray icon and
    // its LEFT edge aligns with the tray icon's LEFT edge — this matches
    // native NSMenu placement (1Password, Bluetooth, etc. all anchor the
    // menu's leading edge to the status item). If aligning that way would
    // push the right edge off-screen, we shift the window leftward to keep
    // it on screen.
    let scale = window.scale_factor().unwrap_or(1.0);
    let icon_pos = rect.position.to_physical::<f64>(scale);
    let outer = window.outer_size().ok();
    if let Some(size) = outer {
        let win_w = size.width as f64;
        let mut x = icon_pos.x;
        // Clamp so the window stays on the same screen the tray icon is on.
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
    // Emit a refresh event so the Svelte side re-pulls armed + prompts.
    let _ = window.emit("tray-popup-show", ());
    let _ = window.show();
    order_panel_front_no_activate(&window);
    #[cfg(target_os = "macos")]
    if let Some(monitor) = app.try_state::<Arc<OutsideClickMonitor>>() {
        install_outside_click_monitor(app, monitor.inner());
    }
}

fn refresh_tray_popup(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("tray-popup") {
        let _ = window.emit("tray-popup-show", ());
    }
}

fn rebuild_match_index(prompts: &[Prompt], matcher: &Arc<MatcherState>) {
    let mut entries = Vec::new();
    for p in prompts {
        for t in &p.triggers {
            entries.push(TriggerEntry {
                canonical: t.to_lowercase(),
                prompt_id: p.id.clone(),
                word_count: t.split_whitespace().count(),
                commit_char: p.commit_char,
            });
        }
    }
    if let Err(e) = matcher.rebuild_index(entries) {
        tracing::error!("trigger conflict: {}", e);
    }
}

fn register_shortcuts(
    app: &mut tauri::App,
    app_state: Arc<AppState>,
    hotkeys: PromptHotkeyMap,
    on_fire: Arc<dyn Fn(Vec<String>, String) + Send + Sync>,
) -> Result<(), Box<dyn std::error::Error>> {
    let shortcut_arm = Shortcut::new(Some(Modifiers::SHIFT | Modifiers::SUPER), Code::KeyP);
    // Command palette — ⌥⌘\ (Option + Cmd + Backslash). Free on stock
    // macOS; not used by Finder, Safari, or major IDEs.
    let shortcut_picker = Shortcut::new(
        Some(Modifiers::ALT | Modifiers::SUPER),
        Code::Backslash,
    );
    let shortcut_kill = Shortcut::new(Some(Modifiers::SHIFT | Modifiers::SUPER), Code::Escape);
    let shortcut_panic = Shortcut::new(Some(Modifiers::SHIFT | Modifiers::SUPER), Code::KeyR);

    let app_state_arm = app_state.clone();
    let app_state_kill = app_state.clone();
    let app_state_panic = app_state.clone();
    let app_handle = app.handle().clone();
    let hotkeys_handler = hotkeys.clone();
    let on_fire_handler = on_fire.clone();

    app.handle().plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_handler(move |_app, shortcut, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                if shortcut == &shortcut_arm {
                    let new = app_state_arm.toggle_armed();
                    tracing::info!("hotkey arm → enabled={}", new);
                    refresh_tray_popup(&app_handle);
                } else if shortcut == &shortcut_picker {
                    // Use the EXACT same code path as the tray menu's
                    // "Command palette…" item, which the user confirmed
                    // works reliably 100% of the time. The only difference
                    // is the positioning is added on top.
                    let app_for_main = app_handle.clone();
                    let _ = app_handle.run_on_main_thread(move || {
                        if let (Some(focus), Some(prompts), Some(index)) = (
                            app_for_main.try_state::<Arc<FocusStore>>(),
                            app_for_main.try_state::<Arc<parking_lot::RwLock<Vec<Prompt>>>>(),
                            app_for_main.try_state::<parking_lot::Mutex<SearchIndex>>(),
                        ) {
                            focus.capture();
                            index.lock().rebuild(&prompts.read());
                        }
                        // Position on cursor's screen first, then use the
                        // same show_window path the tray menu uses.
                        position_picker_on_cursor_screen(&app_for_main);
                        show_window(&app_for_main, "picker");
                    });
                } else if shortcut == &shortcut_kill {
                    tracing::warn!("KILL-SWITCH invoked");
                    app_state_kill.cancel_playback();
                } else if shortcut == &shortcut_panic {
                    tracing::warn!("PANIC-RESET invoked");
                    app_state_panic.cancel_playback();
                    app_state_panic.set_armed(false);
                    if let Ok(mut inj) = prompt_player::inject::EnigoInjector::new() {
                        use prompt_player::typer::Injector;
                        inj.release_all_modifiers();
                    }
                    if let Some(w) = app_handle.get_webview_window("picker") {
                        let _ = w.hide();
                    }
                } else {
                    // Per-prompt hotkey lookup.
                    let map = hotkeys_handler.read();
                    for (hk_str, prompt_id) in map.iter() {
                        if let Ok(s) = Shortcut::from_str(&normalize_hotkey(hk_str)) {
                            if &s == shortcut {
                                tracing::info!("prompt hotkey {} fired → {}", hk_str, prompt_id);
                                (on_fire_handler)(vec![prompt_id.clone()], prompt_id.clone());
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

// Tauri IPC commands ----------------------------------------------------------

#[tauri::command]
fn ipc_get_armed(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state.is_armed()
}

#[tauri::command]
fn ipc_toggle_armed(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state.toggle_armed()
}

#[tauri::command]
fn ipc_kill(state: tauri::State<'_, Arc<AppState>>) {
    state.cancel_playback();
}

#[tauri::command]
fn ipc_set_prompt_enabled(
    prompt_id: String,
    enabled: bool,
    prompts: tauri::State<'_, Arc<parking_lot::RwLock<Vec<Prompt>>>>,
) -> Result<(), String> {
    let prompts_arc: Arc<parking_lot::RwLock<Vec<Prompt>>> = prompts.inner().clone();
    set_prompt_enabled_inner(&prompts_arc, &prompt_id, enabled)
}

/// Open one of the windows referenced from the tray popup. The popup hides
/// itself client-side after firing this so the user gets immediate feedback.
#[tauri::command]
fn ipc_tray_open(app: tauri::AppHandle, target: String) -> Result<(), String> {
    match target.as_str() {
        "library" => {
            show_window(&app, "library");
            Ok(())
        }
        "picker" => {
            if let (Some(focus), Some(prompts), Some(index)) = (
                app.try_state::<Arc<FocusStore>>(),
                app.try_state::<Arc<parking_lot::RwLock<Vec<Prompt>>>>(),
                app.try_state::<parking_lot::Mutex<SearchIndex>>(),
            ) {
                focus.capture();
                index.lock().rebuild(&prompts.read());
            }
            show_window(&app, "picker");
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
        other => Err(format!("unknown tray target: {other}")),
    }
}

#[tauri::command]
fn ipc_tray_quit(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn ipc_picker_dismiss(
    app: tauri::AppHandle,
    focus: tauri::State<'_, Arc<FocusStore>>,
) {
    if let Some(w) = app.get_webview_window("picker") {
        let _ = w.hide();
    }
    // Spotlight-style: restore focus to the app the user was in before
    // summoning the palette so the next keystroke goes back where they
    // expect.
    if !focus.restore() {
        tracing::warn!("focus restore on picker dismiss failed");
    }
}

#[tauri::command]
fn ipc_tray_popup_hide(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("tray-popup") {
        let _ = w.hide();
    }
    #[cfg(target_os = "macos")]
    if let Some(monitor) = app.try_state::<Arc<OutsideClickMonitor>>() {
        remove_outside_click_monitor(monitor.inner());
    }
}

#[tauri::command]
fn ipc_list_prompts(prompts: tauri::State<'_, Arc<parking_lot::RwLock<Vec<Prompt>>>>) -> Vec<Prompt> {
    prompts.read().clone()
}

#[tauri::command]
fn ipc_library_root() -> Result<String, String> {
    library::default_library_root()
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| "could not resolve library root".into())
}

/// Save an existing or new prompt to disk. Hot-reload watcher will pick it up.
#[tauri::command]
fn ipc_save_prompt(prompt: Prompt) -> Result<String, String> {
    let root = library::default_library_root()
        .ok_or_else(|| "could not resolve library root".to_string())?;
    std::fs::create_dir_all(&root).map_err(|e| format!("create dir: {e}"))?;
    let path = match &prompt.source_path {
        Some(p) => p.clone(),
        None => root.join(format!("{}.pp.md", prompt_player::prompts::parser::slugify(&prompt.id))),
    };
    let serialized = prompt_player::prompts::parser::serialize(&prompt)
        .map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, serialized).map_err(|e| format!("write {path:?}: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Create a new prompt with sensible defaults. Returns the saved path.
#[tauri::command]
fn ipc_create_prompt(name: Option<String>) -> Result<Prompt, String> {
    use prompt_player::prompts::parser::slugify;
    let name = name.unwrap_or_else(|| "Untitled prompt".into());
    let mut id = slugify(&name);
    // Ensure unique id by appending a counter if needed.
    let root = library::default_library_root()
        .ok_or_else(|| "could not resolve library root".to_string())?;
    std::fs::create_dir_all(&root).map_err(|e| format!("create dir: {e}"))?;
    let mut path = root.join(format!("{id}.pp.md"));
    let mut n = 1;
    while path.exists() {
        n += 1;
        id = format!("{}-{n}", slugify(&name));
        path = root.join(format!("{id}.pp.md"));
    }
    let prompt = Prompt {
        id: id.clone(),
        name,
        description: String::new(),
        triggers: vec![id.clone()],
        commit_char: '>',
        priority: 0,
        typing_profile: Default::default(),
        typing_overrides: Default::default(),
        scope: None,
        filters: Vec::new(),
        hotkey: None,
        tags: Vec::new(),
        enabled: true,
        body: " your new prompt body here.".into(),
        source_path: Some(path.clone()),
    };
    let serialized = prompt_player::prompts::parser::serialize(&prompt)
        .map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, serialized).map_err(|e| format!("write {path:?}: {e}"))?;
    Ok(prompt)
}

#[tauri::command]
fn ipc_delete_prompt(prompt_id: String, prompts: tauri::State<'_, Arc<parking_lot::RwLock<Vec<Prompt>>>>) -> Result<(), String> {
    let path = {
        let all = prompts.read();
        let p = all
            .iter()
            .find(|p| p.id == prompt_id)
            .ok_or_else(|| format!("prompt {prompt_id} not found"))?;
        p.source_path.clone()
    };
    if let Some(p) = path {
        std::fs::remove_file(&p).map_err(|e| format!("remove {p:?}: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
fn ipc_picker_open(
    app: tauri::AppHandle,
    focus: tauri::State<'_, Arc<FocusStore>>,
    index: tauri::State<'_, parking_lot::Mutex<SearchIndex>>,
    prompts: tauri::State<'_, Arc<parking_lot::RwLock<Vec<Prompt>>>>,
) -> Result<(), String> {
    // Snapshot the foreground app BEFORE we steal focus.
    focus.capture();
    // Rebuild the search index from current prompts.
    {
        let mut idx = index.lock();
        idx.rebuild(&prompts.read());
    }
    position_picker_on_cursor_screen(&app);
    show_window(&app, "picker");
    Ok(())
}

#[tauri::command]
fn ipc_picker_search(
    q: String,
    limit: Option<usize>,
    index: tauri::State<'_, parking_lot::Mutex<SearchIndex>>,
    prompts: tauri::State<'_, Arc<parking_lot::RwLock<Vec<Prompt>>>>,
) -> Vec<SearchHit> {
    let limit = limit.unwrap_or(50);
    let mut idx = index.lock();
    idx.rebuild(&prompts.read());
    idx.query(&q, limit)
}

#[tauri::command]
fn ipc_picker_select(
    app: tauri::AppHandle,
    prompt_id: String,
    mode: String,
    focus: tauri::State<'_, Arc<FocusStore>>,
    prompts: tauri::State<'_, Arc<parking_lot::RwLock<Vec<Prompt>>>>,
    app_state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let prompt = prompts
        .read()
        .iter()
        .find(|p| p.id == prompt_id)
        .cloned()
        .ok_or_else(|| format!("prompt {} not found", prompt_id))?;
    if let Some(w) = app.get_webview_window("picker") {
        let _ = w.hide();
    }
    if !focus.restore() {
        tracing::warn!("focus restore failed");
    }
    std::thread::sleep(RESTORATION_DELAY);
    let app_state_owned: Arc<AppState> = app_state.inner().clone();
    let cancel = app_state_owned.begin_playback();
    let app_state2 = app_state_owned.clone();
    std::thread::Builder::new()
        .name("prompt-player-picker-typer".into())
        .spawn(move || {
            let ctx = PlaceholderContext::default();
            let expanded = expand(&prompt.body, &ctx);
            let mut profile = prompt.effective_profile();
            // Modifier-on-Enter from §5.3.
            // mode = "human" | "fast" | "paste" | "run"
            let final_enter = mode == "run";
            let opts = ScheduleOptions {
                rdp_mode: false,
                include_pre_typing_pause: false,
            };
            profile.send_final_enter = final_enter;
            if mode == "fast" {
                profile.iki_scale = 0.20;
                profile.typos_enabled = false;
                profile.pre_submit_pause_enabled = false;
            }
            let mut rng = ChaCha8Rng::from_entropy();
            match mode.as_str() {
                "paste" => {
                    // Paste via clipboard: instant; breaks the illusion but matches §5.3.
                    if let Ok(mut inj) = EnigoInjector::new() {
                        use prompt_player::typer::Injector;
                        for c in expanded.text.chars() {
                            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                inj.release_all_modifiers();
                                app_state2.end_playback();
                                return;
                            }
                            inj.type_char(c);
                        }
                        if final_enter {
                            inj.press_enter();
                        }
                    }
                }
                _ => {
                    let s = schedule(&expanded.text, &profile, &opts, &mut rng);
                    if let Ok(mut inj) = EnigoInjector::new() {
                        let _ = play(&s, &mut inj, cancel);
                    }
                }
            }
            app_state2.end_playback();
        })
        .expect("spawn picker typer thread");
    Ok(())
}
