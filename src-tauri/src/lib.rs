//! Prompt Player — stealth keyboard utility for live demos.
//!
//! Re-exports for tests and the `typing-engine-cli` binary.

// Legacy `objc` macros emit `cargo-clippy` cfg probes that Rust's check-cfg
// sees at our call sites. Keep validation output focused until those small
// macOS shims finish moving to `objc2`.
#![allow(unexpected_cfgs)]

pub mod typer;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod inject;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod hook;

pub mod accessibility;
pub mod app;
pub mod commands;
pub mod config;
pub mod error;
pub mod filters;
pub mod hotkey;
pub mod matcher;
pub mod platform;
pub mod power;
pub mod prompts;
pub mod rdp;
pub mod repo;
pub mod scopes;
pub mod sources;
pub mod state;
pub mod store;
pub mod telemetry;
pub mod undo;
pub mod usage;

#[cfg(target_os = "macos")]
pub mod secure_input;

#[cfg(target_os = "macos")]
pub mod tcc;

pub mod picker;
