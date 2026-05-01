//! Cross-platform window-chrome / native-OS surface. All `unsafe` Cocoa /
//! Win32 calls live behind the modules in here so the rest of the codebase
//! stays platform-agnostic.

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;
