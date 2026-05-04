//! Windows window-chrome / Win32 panel surface — mirrors the macOS module's
//! public API so call sites can be cfg-driven without per-platform branches.
//!
//! All `unsafe` Win32 calls live behind these submodules.

pub mod activation;
pub mod menu;
pub mod panel;
pub mod screen;
pub mod taskbar;
pub mod tray_theme;

pub use activation::{activate_app, order_panel_front_no_activate};
pub use menu::show_tray_menu;
pub use panel::{configure_picker_window, configure_popover_window, make_window_space_neutral};
pub use screen::{position_centered_on_cursor, position_picker_on_cursor_screen};
pub use taskbar::{taskbar_edge, TaskbarEdge};
pub use tray_theme::{
    install_theme_watcher as install_tray_theme_watcher, pick_icon_bytes as pick_tray_icon_bytes,
};
