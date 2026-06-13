//! macOS-specific window-chrome / NSEvent monitor / activation surface.
//!
//! All `unsafe` Cocoa calls live in this subtree. New code uses the modern
//! `objc2` ecosystem; legacy `cocoa`/`objc` is still permitted for code we
//! haven't migrated yet.

#![allow(deprecated, unexpected_cfgs)]

pub mod activation;
pub mod monitor;
pub mod nsworkspace;
pub mod panel;
pub mod screen;

pub use activation::{activate_app, order_panel_front_no_activate};
pub use monitor::{
    install_outside_click_monitor, remove_outside_click_monitor, OutsideClickMonitor,
};
pub use panel::{
    configure_picker_window, configure_popover_window, make_window_space_neutral,
    order_window_front_regardless,
};
pub use screen::{
    position_centered_on_cursor, position_picker_on_cursor_screen, position_popover_under_cursor,
};
