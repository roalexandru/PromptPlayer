//! Tauri IPC commands. Each domain lives in its own submodule. The
//! `generate_handler!` aggregation lives in `app::setup`.

pub mod armed;
pub mod config;
pub mod diagnostics;
pub mod library;
pub mod picker;
pub mod power;
pub mod prompts;
pub mod shell;
pub mod sources;
pub mod tray;
pub mod updater;

/// Single source of truth for the command list; `generate_handler!` and
/// `collect_commands!` must match it in order, and a test enforces that.
pub const COMMAND_NAMES: &[&str] = &[
    "get_armed",
    "toggle_armed",
    "kill",
    "is_playing",
    "is_hook_alive",
    "open_accessibility_settings",
    "reset_accessibility",
    "get_keep_awake",
    "toggle_keep_awake",
    "set_keep_awake_duration",
    "set_keep_awake_restore",
    "get_diagnostics",
    "run_self_test",
    "self_test_type",
    "open_diagnostics",
    "get_settings",
    "set_restore_armed",
    "list_prompts",
    "library_root",
    "save_prompt",
    "create_prompt",
    "delete_prompt",
    "set_prompt_enabled",
    "set_prompt_pinned",
    "picker_open",
    "picker_search",
    "picker_select",
    "picker_dismiss",
    "tray_open",
    "tray_quit",
    "tray_popup_hide",
    "tray_fire_prompt",
    "updater_current_version",
    "updater_check",
    "updater_install",
    "updater_announced",
    "updater_dismiss",
    "capture_foreground_app",
    "expand_prompt_text",
    "import_prompt",
    "export_prompt",
    "open_external",
    "get_config",
    "save_config",
    "get_setlist",
    "set_setlist",
    "fire_next_cue",
    "reset_setlist",
    "playback_status",
    "toggle_playback_pause",
    "nudge_playback_speed",
    "prompt_stops",
    "list_sources",
    "add_source",
    "remove_source",
    "refresh_sources",
    "set_remote_prompt_enabled",
    "fork_prompt",
    "import_agent_prompts",
    "agent_import_candidates",
    "capture_last_typed",
    "source_pending_changes",
    "apply_source_updates",
];

#[cfg(test)]
mod registry_tests {
    //! Registry cross-checks with no Tauri runtime dependency, so they run on
    //! every platform. Catches drift between `COMMAND_NAMES` and the macros.
    use super::COMMAND_NAMES;

    const SETUP_RS: &str = include_str!("../app/setup.rs");

    fn extract_macro_paths(src: &str, macro_name: &str) -> Vec<String> {
        let needle = format!("{}![", macro_name);
        let start = src.find(&needle).unwrap_or_else(|| {
            panic!("macro `{}` not found in setup.rs", macro_name);
        }) + needle.len();
        let rest = &src[start..];
        let end = rest.find(']').expect("unterminated macro invocation");
        let body = &rest[..end];

        body.split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && !s.starts_with("//"))
            .map(|s| s.split("::").last().unwrap().trim().to_string())
            .collect()
    }

    #[test]
    fn every_configured_window_has_a_capability() {
        // A window absent from every capability gets zero permissions, so its
        // `core:window:*` calls are denied at runtime — silently, when the
        // caller swallows the rejection. Diagnostics shipped that way: its own
        // Close button and Esc were no-ops.
        const CONF: &str = include_str!("../../tauri.conf.json");
        const CAPS: [&str; 4] = [
            include_str!("../../capabilities/default.json"),
            include_str!("../../capabilities/library.json"),
            include_str!("../../capabilities/picker.json"),
            include_str!("../../capabilities/tray-popup.json"),
        ];
        let mut covered: Vec<String> = Vec::new();
        for cap in CAPS {
            let v: serde_json::Value = serde_json::from_str(cap).expect("capability json");
            if let Some(ws) = v["windows"].as_array() {
                covered.extend(ws.iter().filter_map(|w| w.as_str().map(str::to_string)));
            }
        }
        for label in window_labels(CONF) {
            assert!(
                covered.contains(&label),
                "window '{label}' is in tauri.conf.json but in no capabilities/*.json \
                 `windows` list — it will be denied every core:window permission"
            );
        }
    }

    #[test]
    fn every_configured_window_gets_lifecycle_and_chrome() {
        // Missing from `lifecycle::install`, a window is destroyed on close
        // instead of hidden, and `get_webview_window` returns None forever
        // after — the tray item that opens it becomes a one-shot.
        const CONF: &str = include_str!("../../tauri.conf.json");
        const LIFECYCLE_RS: &str = include_str!("../app/lifecycle.rs");
        for label in window_labels(CONF) {
            let quoted = format!("\"{label}\"");
            assert!(
                LIFECYCLE_RS.contains(&quoted),
                "window '{label}' has no handler in lifecycle::install — closing it \
                 destroys it instead of hiding it"
            );
            assert!(
                SETUP_RS.contains(&quoted),
                "window '{label}' never passes through apply_window_chrome"
            );
        }
    }

    /// Window labels declared in `tauri.conf.json`.
    fn window_labels(conf: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(conf).expect("tauri.conf.json");
        let labels: Vec<String> = v["app"]["windows"]
            .as_array()
            .expect("app.windows")
            .iter()
            .filter_map(|w| w["label"].as_str().map(str::to_string))
            .collect();
        assert!(!labels.is_empty(), "no windows found in tauri.conf.json");
        labels
    }

    #[test]
    fn generate_handler_list_matches_command_names() {
        let registered = extract_macro_paths(SETUP_RS, "tauri::generate_handler");
        let expected: Vec<String> = COMMAND_NAMES.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            registered, expected,
            "the `generate_handler!` list in setup.rs has drifted from \
             `commands::COMMAND_NAMES`. Add the new command name to \
             COMMAND_NAMES (or remove it from the macro) so all three \
             registries stay in lockstep."
        );
    }

    #[test]
    fn collect_commands_list_matches_command_names() {
        let registered = extract_macro_paths(SETUP_RS, "collect_commands");
        let expected: Vec<String> = COMMAND_NAMES.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            registered, expected,
            "the `collect_commands!` (tauri-specta) list has drifted from \
             `commands::COMMAND_NAMES`. The generated TypeScript bindings \
             will be missing — or have stale — entries until this is fixed."
        );
    }

    #[test]
    fn manage_state_helper_matches_inline_block_in_run() {
        fn extract_manage_chain(src: &str, marker: &str) -> Vec<String> {
            let chunk_start = src
                .find(marker)
                .unwrap_or_else(|| panic!("missing marker {marker}"));
            let chunk_end = (chunk_start + 1500).min(src.len());
            let chunk = &src[chunk_start..chunk_end];
            let mut managed = Vec::new();
            for line in chunk.lines() {
                let l = line.trim();
                for prefix in [".manage(", "builder.manage("] {
                    if let Some(rest) = l.strip_prefix(prefix) {
                        let bytes = rest.as_bytes();
                        let mut depth = 1i32;
                        let mut e = 0;
                        while e < bytes.len() {
                            match bytes[e] {
                                b'(' => depth += 1,
                                b')' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            e += 1;
                        }
                        let raw = rest[..e].trim();
                        let normalized = raw.strip_suffix(".clone()").unwrap_or(raw);
                        managed.push(normalized.to_string());
                        break;
                    }
                }
                if l == "}" && !managed.is_empty() {
                    break;
                }
            }
            managed
        }
        let inline = extract_manage_chain(SETUP_RS, "// Per-state managed handles");
        let helper = extract_manage_chain(SETUP_RS, "pub fn manage_state<R: tauri::Runtime>");
        assert_eq!(
            inline, helper,
            "the inline `.manage()` block in `run()` has drifted from \
             `manage_state()`. Add the new type to BOTH places (or remove \
             from both)."
        );
    }
}
