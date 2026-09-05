//! Which screen edge the taskbar is on, so the tray popup grows away from it
//! like the native Quick Settings flyouts do.
//!
//! `SHAppBarMessage(ABM_GETTASKBARPOS)` answers for the primary taskbar, whose
//! edge dictates icon orientation even on mirrored multi-monitor setups.

use std::mem;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETTASKBARPOS, APPBARDATA,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskbarEdge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Return the screen edge the Windows taskbar is anchored to. Falls back to
/// `Bottom` (the Win10/11 default) if the API call fails.
pub fn taskbar_edge() -> TaskbarEdge {
    let mut data = APPBARDATA::default();
    data.cbSize = mem::size_of::<APPBARDATA>() as u32;
    let result = unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) };
    if result == 0 {
        return TaskbarEdge::Bottom;
    }
    match data.uEdge {
        e if e == ABE_TOP => TaskbarEdge::Top,
        e if e == ABE_BOTTOM => TaskbarEdge::Bottom,
        e if e == ABE_LEFT => TaskbarEdge::Left,
        e if e == ABE_RIGHT => TaskbarEdge::Right,
        _ => TaskbarEdge::Bottom,
    }
}
