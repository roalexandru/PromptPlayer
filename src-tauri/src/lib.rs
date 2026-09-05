//! Prompt Player — stealth keyboard utility for live demos.
//!
//! Re-exports for tests and the `typing-engine-cli` binary.

// Legacy `objc` macros emit `cargo-clippy` cfg probes at our call sites.
// Silenced until the last macOS shims move to `objc2`.
#![allow(unexpected_cfgs)]

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
pub mod power;
pub mod prompts;
pub mod rdp;
pub mod scopes;
pub mod settings;
pub mod state;
pub mod store;
pub mod telemetry;
pub mod tray_icon;
pub mod undo;

// Not cfg-gated: both carry `cfg(not(macos))` stubs for cross-platform callers,
// and gating the modules made those stubs unreachable.
pub mod secure_input;
pub mod tcc;

pub mod picker;
