//! Tauri IPC commands. Each domain lives in its own submodule. The
//! `generate_handler!` aggregation lives in `app::setup`.

pub mod armed;
pub mod library;
pub mod picker;
pub mod prompts;
pub mod shell;
pub mod tray;
pub mod updater;

/// Single source of truth for the command name list.
///
/// Every entry here MUST appear in:
/// - `tauri::generate_handler![...]` in `app::setup::register_commands_and_state`
/// - `tauri_specta::collect_commands![...]` in `app::setup::generate_typescript_bindings`
///
/// The test in `tests/ipc_registry.rs` cross-checks all three lists at every
/// `cargo test` run, so adding a command without registering it everywhere
/// fails CI before it can ship.
pub const COMMAND_NAMES: &[&str] = &[
    "get_armed",
    "toggle_armed",
    "kill",
    "is_playing",
    "is_hook_alive",
    "open_accessibility_settings",
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
    "capture_foreground_app",
    "expand_prompt_text",
    "import_prompt",
    "export_prompt",
    "open_external",
];

#[cfg(test)]
mod registry_tests {
    //! Cross-check tests for the IPC command registry. Run on every
    //! platform via `cargo test` (no Tauri runtime dependency, unlike
    //! `tests/ipc_registry.rs`). Catches drift between `COMMAND_NAMES`
    //! and the macro lists in `app/setup.rs`.
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
