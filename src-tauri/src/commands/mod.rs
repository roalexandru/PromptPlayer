//! Tauri IPC commands. Each domain lives in its own submodule. The
//! `generate_handler!` aggregation lives in `app::setup`.

pub mod armed;
pub mod picker;
pub mod prompts;
pub mod tray;

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
    "list_prompts",
    "library_root",
    "save_prompt",
    "create_prompt",
    "delete_prompt",
    "set_prompt_enabled",
    "picker_open",
    "picker_search",
    "picker_select",
    "picker_dismiss",
    "tray_open",
    "tray_quit",
    "tray_popup_hide",
];
