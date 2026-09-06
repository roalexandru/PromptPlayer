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
    /// §5.4 exclusion is not fully in effect, so the picker may show up in a
    /// screen share. The one state a presenter must not learn from a log file.
    capture_degraded: AtomicBool,
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

    pub fn set_capture_degraded(&self, degraded: bool) -> bool {
        self.capture_degraded.swap(degraded, Ordering::Relaxed) != degraded
    }

    pub fn capture_degraded(&self) -> bool {
        self.capture_degraded.load(Ordering::Relaxed)
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
    // Always a template. `tray-icon.png` is a pure-black glyph carried entirely
    // by its alpha channel, so rendering it literally — which is what turning
    // the template flag off does — paints black on a dark menu bar and the icon
    // vanishes, leaving only the badge dot floating there. Template mode is the
    // whole reason a menu-bar glyph is authored black-on-transparent: AppKit
    // recolours it to whatever the bar needs, in either appearance.
    //
    // The cost is that the badge loses its amber on macOS, because a template
    // keeps only alpha. `with_badge` punches a transparent ring around the dot
    // so it still reads as a separate dot once flattened to one colour. Windows
    // ignores the flag, so the badge stays amber there.
    if let Err(e) = tray.set_icon_as_template(true) {
        tracing::warn!("tray set_icon_as_template failed: {}", e);
    }
}

/// Amber, matching the diagnostics banner. Opaque so it survives the alpha
/// blend against whatever the glyph already has in that corner.
const BADGE_RGBA: [u8; 4] = [255, 159, 10, 255];

/// Dot radius as a fraction of the shorter edge. The menu bar draws this glyph
/// at roughly 18pt, so it is sized proportionally rather than in pixels. A
/// quarter — the first attempt — came out half the icon's height and read as
/// part of the logo rather than a badge, which matters more in template mode
/// where colour cannot distinguish the two.
const BADGE_RADIUS_FRACTION: f32 = 0.16;
/// Transparent ring around the dot, as a fraction of its radius.
const BADGE_MOAT_FRACTION: f32 = 0.22;

/// Where the badge sits on a `w` x `h` canvas.
struct Badge {
    cx: f32,
    cy: f32,
    radius: f32,
    moat: f32,
}

/// Shared by the painter and its tests, so neither re-derives the constants.
fn badge_geometry(w: u32, h: u32) -> Badge {
    let radius = ((w.min(h) as f32) * BADGE_RADIUS_FRACTION).round().max(2.0);
    Badge {
        cx: w as f32 - radius - 1.0,
        cy: radius + 1.0,
        radius,
        moat: (radius * BADGE_MOAT_FRACTION).round().max(1.0),
    }
}

/// Paint a filled circle into the top-right corner of an RGBA buffer, or return
/// it unchanged if the dimensions don't match — a decoder surprise can't panic.
///
/// The dot is ringed by a band of fully transparent pixels. macOS renders the
/// tray icon as a template, keeping only alpha, so without that gap a dot that
/// touched the glyph would merge into it and read as part of the artwork rather
/// than as a badge.
pub fn with_badge(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = rgba.to_vec();
    let expected = (w as usize) * (h as usize) * 4;
    if w == 0 || h == 0 || out.len() != expected {
        return out;
    }
    let Badge {
        cx,
        cy,
        radius,
        moat,
    } = badge_geometry(w, h);
    let r2 = radius * radius;
    let outer = radius + moat;
    let outer2 = outer * outer;

    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d2 = dx * dx + dy * dy;
            if d2 > outer2 {
                continue;
            }
            let i = ((y as usize) * (w as usize) + (x as usize)) * 4;
            if d2 <= r2 {
                out[i..i + 4].copy_from_slice(&BADGE_RGBA);
            } else {
                // Clear the glyph out of the ring, badge colour included.
                out[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
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
        let g = badge_geometry(w, h);
        assert_eq!(px(&out, w, g.cx as u32, g.cy as u32), BADGE_RGBA);
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
    fn the_badge_never_turns_the_template_flag_off() {
        // `tray-icon.png` is a pure-black glyph carried by its alpha channel.
        // Rendered non-template it paints black on a dark menu bar and the icon
        // disappears, leaving a bare dot — which is exactly what shipped.
        const SRC: &str = include_str!("tray_icon.rs");
        let start = SRC.find("pub fn refresh(").expect("refresh");
        let body = &SRC[start..];
        let body = &body[..body.find("\n}").expect("fn end")];
        assert!(
            body.contains("set_icon_as_template(true)"),
            "the tray icon must stay a template; a black glyph rendered \
             literally is invisible on a dark menu bar"
        );
        assert!(
            !body.contains("set_icon_as_template(!"),
            "the template flag must not depend on the badge"
        );
    }

    #[test]
    fn the_base_glyph_is_black_and_needs_template_rendering() {
        // The premise of the test above, asserted against the real asset: if
        // the artwork is ever redrawn with its own colours, this fails and the
        // template decision deserves revisiting.
        let img = tauri::image::Image::from_bytes(base_bytes()).expect("decode tray icon");
        let opaque: Vec<&[u8]> = img.rgba().chunks_exact(4).filter(|p| p[3] > 10).collect();
        assert!(!opaque.is_empty(), "tray icon has no visible pixels");
        assert!(
            opaque.iter().all(|p| p[0] < 40 && p[1] < 40 && p[2] < 40),
            "tray glyph is no longer black — it may no longer need template mode"
        );
    }

    #[test]
    fn a_transparent_ring_separates_the_badge_from_the_glyph() {
        // Template rendering keeps only alpha, so a dot touching the glyph
        // merges into it. Fill the canvas and check the ring is punched clear.
        let (w, h) = (88u32, 88u32);
        let solid = vec![255u8; (w * h * 4) as usize];
        let out = with_badge(&solid, w, h);

        let Badge {
            cx,
            cy,
            radius: r,
            moat,
        } = badge_geometry(w, h);

        // Probe the inner edges — the dot sits flush against the top-right
        // corner, so those two sides have no canvas left for a moat, and the
        // gap that matters is the one facing the glyph anyway.
        let left = (cx - r - moat / 2.0) as u32;
        assert_eq!(
            px(&out, w, left, cy as u32)[3],
            0,
            "no transparent gap to the left of the badge"
        );
        let below = (cy + r + moat / 2.0) as u32;
        assert_eq!(
            px(&out, w, cx as u32, below)[3],
            0,
            "no transparent gap below the badge"
        );
        // The dot itself is still opaque.
        assert_eq!(px(&out, w, cx as u32, cy as u32)[3], 255);
        // And the far corner is untouched.
        assert_eq!(px(&out, w, 0, h - 1), [255, 255, 255, 255]);
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
