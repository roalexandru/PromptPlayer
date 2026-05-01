//! Prompt Player — stealth keyboard utility for live demos.
//!
//! Re-exports for tests and the `typing-engine-cli` binary.

pub mod typer;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod inject;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod hook;

pub mod app;
pub mod commands;
pub mod error;
pub mod filters;
pub mod hotkey;
pub mod matcher;
pub mod platform;
pub mod prompts;
pub mod rdp;
pub mod scopes;
pub mod state;
pub mod store;
pub mod telemetry;
pub mod undo;

#[cfg(target_os = "macos")]
pub mod secure_input;

#[cfg(target_os = "macos")]
pub mod tcc;

pub mod picker;
