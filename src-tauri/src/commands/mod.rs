//! Tauri IPC commands. Each domain lives in its own submodule. The
//! `generate_handler!` aggregation lives in `app::setup`.

pub mod armed;
pub mod picker;
pub mod prompts;
pub mod tray;
