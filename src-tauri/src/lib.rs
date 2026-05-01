//! Prompt Player — stealth keyboard utility for live demos.
//!
//! Re-exports for tests and the `typing-engine-cli` binary.

pub mod typer;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod inject;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod hook;

pub mod matcher;
pub mod prompts;
pub mod scopes;
pub mod filters;
pub mod rdp;
pub mod telemetry;
pub mod undo;
pub mod state;

#[cfg(target_os = "macos")]
pub mod secure_input;

#[cfg(target_os = "macos")]
pub mod tcc;

pub mod picker;
pub mod ipc;
