//! §2.7 — flash the tray icon red when the kill-switch fires.
//!
//! The spec lists three things the kill-switch does: stop the typing thread,
//! release all modifiers, and "briefly flash the tray icon red". The first two
//! shipped; this is the third. It is the only feedback the user gets that an
//! abort actually landed — cancellation is deliberately silent (§3.5: no
//! popup, no notification), so without it a kill that failed and a kill that
//! worked look identical from the stage.
//!
//! The red icon is derived from the tray icon already baked into the binary
//! rather than shipped as a second asset, so it automatically follows the
//! per-platform icon choice (macOS template glyph, Windows light/dark variant).

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::image::Image;
use tauri::AppHandle;

/// How long the icon stays red. Long enough to register in peripheral vision,
/// short enough not to look like a stuck error state.
const FLASH_MS: u64 = 450;

/// Apple's `systemRed`, which is also close enough to Windows' danger red that
/// one constant serves both.
const RED: [u8; 3] = [255, 59, 48];

/// Only one flash at a time. Three panic keystrokes fire three kills in quick
/// succession (§2.6), and overlapping flashes would fight over the icon and
/// could leave it red after the last one restores.
static FLASHING: AtomicBool = AtomicBool::new(false);

/// Tint every non-transparent pixel red, preserving alpha so the glyph's shape
/// and antialiasing survive.
fn tinted_red(source: &Image<'_>) -> Image<'static> {
    let mut rgba = source.rgba().to_vec();
    for px in rgba.chunks_exact_mut(4) {
        // Fully transparent pixels stay transparent; tinting them would turn
        // the icon into a red square.
        if px[3] == 0 {
            continue;
        }
        px[0] = RED[0];
        px[1] = RED[1];
        px[2] = RED[2];
    }
    Image::new_owned(rgba, source.width(), source.height())
}

/// Flash the tray icon red, then restore it. Returns immediately; the flash
/// runs on its own thread so the kill path is never delayed by it.
pub fn flash_kill(app: &AppHandle) {
    if FLASHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("prompt-player-tray-flash".into())
        .spawn(move || {
            flash_blocking(&app);
            FLASHING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        // Couldn't spawn — clear the guard so a later kill can still flash.
        FLASHING.store(false, Ordering::Release);
    }
}

fn flash_blocking(app: &AppHandle) {
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let base_bytes = crate::app::setup::tray_icon_bytes();
    let Ok(base) = Image::from_bytes(base_bytes) else {
        tracing::debug!("tray flash: could not decode the tray icon");
        return;
    };
    let red = tinted_red(&base);
    if let Err(e) = tray.set_icon(Some(red)) {
        tracing::debug!("tray flash: set_icon failed: {}", e);
        return;
    }
    // macOS tints template images to match the menu bar, which would undo the
    // red. Turn that off for the duration of the flash.
    #[cfg(target_os = "macos")]
    let _ = tray.set_icon_as_template(false);

    std::thread::sleep(std::time::Duration::from_millis(FLASH_MS));

    #[cfg(target_os = "macos")]
    let _ = tray.set_icon_as_template(true);
    // Re-read the bytes rather than reusing `base`: on Windows the theme
    // watcher may have swapped light/dark while we were red.
    if let Ok(restored) = Image::from_bytes(crate::app::setup::tray_icon_bytes()) {
        if let Err(e) = tray.set_icon(Some(restored)) {
            tracing::warn!("tray flash: could not restore the icon: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tint_recolors_opaque_pixels_and_keeps_alpha() {
        // 2x1: one opaque green pixel, one fully transparent.
        let src = Image::new_owned(vec![0, 200, 0, 255, 1, 2, 3, 0], 2, 1);
        let out = tinted_red(&src);
        let px = out.rgba();
        assert_eq!(&px[0..4], &[RED[0], RED[1], RED[2], 255]);
        assert_eq!(
            &px[4..8],
            &[1, 2, 3, 0],
            "transparent pixels are left alone, or the icon becomes a red square"
        );
    }

    #[test]
    fn tint_preserves_dimensions() {
        let src = Image::new_owned(vec![0; 4 * 6], 3, 2);
        let out = tinted_red(&src);
        assert_eq!((out.width(), out.height()), (3, 2));
        assert_eq!(out.rgba().len(), 4 * 6);
    }

    #[test]
    fn tint_preserves_partial_alpha_for_antialiased_edges() {
        let src = Image::new_owned(vec![10, 20, 30, 128], 1, 1);
        let out = tinted_red(&src);
        assert_eq!(out.rgba()[3], 128, "antialiasing must survive the tint");
    }

    #[test]
    fn the_real_tray_icon_can_be_tinted() {
        // Guards the baked-in asset actually decoding — a bad include_bytes!
        // path would otherwise only surface as a silent no-op at runtime.
        let base = Image::from_bytes(crate::app::setup::tray_icon_bytes())
            .expect("bundled tray icon decodes");
        let out = tinted_red(&base);
        assert_eq!((out.width(), out.height()), (base.width(), base.height()));
    }
}
