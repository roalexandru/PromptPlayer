//! Tray-icon rendering, including the attention badge.
//!
//! The tray icon is the only always-visible surface, so it carries the two
//! states the user must not miss: a dead keyboard hook, and an available
//! update that sat unnoticed for days in the field data.

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::image::Image;
use tauri::{AppHandle, Manager};

/// Tray icon id, set by `TrayIconBuilder::with_id` in `app::setup`.
pub const TRAY_ID: &str = "main";

#[cfg(not(target_os = "windows"))]
const BASE_ICON: &[u8] = include_bytes!("../icons/tray-icon.png");

/// What the tray icon should be shouting about, if anything.
#[derive(Debug, Default)]
pub struct Attention {
    /// The keyboard hook isn't running, so no trigger can fire.
    hook_dead: AtomicBool,
    /// An undismissed update is available.
    update: AtomicBool,
}

impl Attention {
    pub fn shared() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    /// Returns true when the value changed, so callers only redraw on edges.
    pub fn set_hook_dead(&self, dead: bool) -> bool {
        self.hook_dead.swap(dead, Ordering::Relaxed) != dead
    }

    pub fn set_update(&self, available: bool) -> bool {
        self.update.swap(available, Ordering::Relaxed) != available
    }

    pub fn needs_badge(&self) -> bool {
        self.hook_dead.load(Ordering::Relaxed) || self.update.load(Ordering::Relaxed)
    }
}

/// Base icon bytes for the current platform and OS theme.
pub fn base_bytes() -> &'static [u8] {
    #[cfg(target_os = "windows")]
    {
        crate::platform::windows::pick_tray_icon_bytes()
    }
    #[cfg(not(target_os = "windows"))]
    {
        BASE_ICON
    }
}

/// Redraw the tray icon for the current theme and attention state. Safe to
/// call from any thread and at any time; a missing tray is a no-op.
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let badge = app
        .try_state::<crate::app::context::AppContext>()
        .map(|ctx| ctx.attention.needs_badge())
        .unwrap_or(false);

    let Ok(base) = Image::from_bytes(base_bytes()) else {
        tracing::warn!("tray icon bytes failed to decode");
        return;
    };
    let (w, h) = (base.width(), base.height());
    let icon = if badge {
        Image::new_owned(with_badge(base.rgba(), w, h), w, h)
    } else {
        Image::new_owned(base.rgba().to_vec(), w, h)
    };

    if let Err(e) = tray.set_icon(Some(icon)) {
        tracing::warn!("tray set_icon failed: {}", e);
        return;
    }
    // Not a template while badged, or AppKit flattens the dot into the glyph.
    // After the icon, so it lands on the image we just set.
    if let Err(e) = tray.set_icon_as_template(!badge) {
        tracing::warn!("tray set_icon_as_template failed: {}", e);
    }
}

/// Amber, matching the diagnostics banner. Opaque so it survives the alpha
/// blend against whatever the glyph already has in that corner.
const BADGE_RGBA: [u8; 4] = [255, 159, 10, 255];

/// Paint a filled circle into the top-right corner of an RGBA buffer, or return
/// it unchanged if the dimensions don't match — a decoder surprise can't panic.
pub fn with_badge(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = rgba.to_vec();
    let expected = (w as usize) * (h as usize) * 4;
    if w == 0 || h == 0 || out.len() != expected {
        return out;
    }
    // Quarter of the shorter edge, floored at 2px so tiny icons still show it.
    let radius = ((w.min(h) as f32) * 0.25).round().max(2.0);
    let cx = w as f32 - radius - 1.0;
    let cy = radius + 1.0;
    let r2 = radius * radius;

    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            if dx * dx + dy * dy > r2 {
                continue;
            }
            let i = ((y as usize) * (w as usize) + (x as usize)) * 4;
            out[i..i + 4].copy_from_slice(&BADGE_RGBA);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(w: u32, h: u32) -> Vec<u8> {
        vec![0u8; (w * h * 4) as usize]
    }

    fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y as usize) * (w as usize) + (x as usize)) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn badge_paints_the_top_right_and_leaves_the_rest_alone() {
        let (w, h) = (32, 32);
        let out = with_badge(&blank(w, h), w, h);
        assert_eq!(out.len(), (w * h * 4) as usize, "size must be preserved");
        // Corner opposite the badge is untouched.
        assert_eq!(px(&out, w, 0, h - 1), [0, 0, 0, 0]);
        // A pixel at the badge centre is opaque amber.
        let r = ((w.min(h) as f32) * 0.25).round();
        let (cx, cy) = ((w as f32 - r - 1.0) as u32, (r + 1.0) as u32);
        assert_eq!(px(&out, w, cx, cy), BADGE_RGBA);
    }

    #[test]
    fn badge_stays_inside_the_bitmap() {
        // Every painted pixel must be in-bounds; the loop indexes by (x, y), so
        // an off-by-one in the centre maths would panic rather than clip.
        for size in [16u32, 22, 32, 44, 64] {
            let out = with_badge(&blank(size, size), size, size);
            assert_eq!(out.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn badge_is_a_visible_fraction_of_the_icon() {
        let (w, h) = (32, 32);
        let out = with_badge(&blank(w, h), w, h);
        let painted = out.chunks_exact(4).filter(|p| p[3] == 255).count();
        let total = (w * h) as usize;
        // Big enough to notice in a menu bar, small enough to leave the glyph.
        assert!(painted > total / 100, "badge too small: {painted}/{total}");
        assert!(painted < total / 4, "badge too large: {painted}/{total}");
    }

    #[test]
    fn malformed_buffers_pass_through_untouched() {
        // A decoder surprise must never panic the tray.
        let short = vec![1u8, 2, 3];
        assert_eq!(with_badge(&short, 32, 32), short);
        assert!(with_badge(&[], 0, 0).is_empty());
    }

    #[test]
    fn attention_reports_only_edges() {
        let a = Attention::default();
        assert!(!a.needs_badge());
        assert!(a.set_hook_dead(true), "false -> true is an edge");
        assert!(!a.set_hook_dead(true), "no edge on a repeat");
        assert!(a.needs_badge());

        // Either flag alone keeps the badge lit.
        assert!(a.set_update(true));
        assert!(a.set_hook_dead(false));
        assert!(a.needs_badge(), "update alone still warrants a badge");
        assert!(a.set_update(false));
        assert!(!a.needs_badge());
    }
}
