//! macOS-specific keystroke synthesis notes.
//!
//! `enigo` already uses `CGEventCreateKeyboardEvent`; for Phase 1 we rely on it
//! directly via `inject::EnigoInjector`. Phase 9 may add a Unicode-string fast-path
//! for runs of non-ASCII chars where `CGEventKeyboardSetUnicodeString` is faster
//! than per-key events.
//!
//! `paste_via_clipboard` writes the body to `NSPasteboard.generalPasteboard`,
//! synthesizes Cmd+V via CGEvent, waits for the target app to consume the
//! paste, then restores the prior pasteboard text. Same shape as the Windows
//! impl; see `inject/mod.rs` for the cross-platform entry point.

use super::PasteError;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc2::rc::Retained;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;
use std::time::Duration;

const KC_V: CGKeyCode = 9; // ANSI_V

pub(super) fn paste_via_clipboard(body: &str) -> Result<(), PasteError> {
    // 1. Snapshot current pasteboard text.
    let saved = read_pasteboard_string();

    // 2. Set ours.
    if let Err(e) = write_pasteboard_string(body) {
        if let Some(s) = saved {
            let _ = write_pasteboard_string(&s);
        }
        return Err(PasteError::Clipboard(e));
    }

    // 3. Synthesize Cmd+V.
    if let Err(e) = synth_cmd_v() {
        if let Some(s) = saved {
            let _ = write_pasteboard_string(&s);
        }
        return Err(PasteError::Injection(e));
    }

    // 4. Let the paste land before we overwrite the pasteboard.
    std::thread::sleep(Duration::from_millis(60));

    // 5. Restore.
    if let Some(s) = saved {
        if let Err(e) = write_pasteboard_string(&s) {
            tracing::warn!("pasteboard restore failed: {}", e);
        }
    }
    Ok(())
}

fn read_pasteboard_string() -> Option<String> {
    unsafe {
        let pb: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
        let nsstr = pb.stringForType(NSPasteboardTypeString)?;
        Some(nsstr.to_string())
    }
}

fn write_pasteboard_string(text: &str) -> Result<(), String> {
    unsafe {
        let pb: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let ns_text: Retained<NSString> = NSString::from_str(text);
        // `setString:forType:` is the simplest writer that handles a single
        // textual flavor; we don't need writeObjects:'s array shape for one
        // string. Returns BOOL: NO on failure (extremely rare in practice).
        if pb.setString_forType(&ns_text, NSPasteboardTypeString) {
            Ok(())
        } else {
            Err("NSPasteboard setString:forType: returned NO".into())
        }
    }
}

fn synth_cmd_v() -> Result<(), String> {
    let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "CGEventSource::new failed".to_string())?;
    let down = CGEvent::new_keyboard_event(src.clone(), KC_V, true)
        .map_err(|_| "CGEvent down create failed".to_string())?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    let up = CGEvent::new_keyboard_event(src, KC_V, false)
        .map_err(|_| "CGEvent up create failed".to_string())?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);
    up.post(CGEventTapLocation::HID);
    Ok(())
}
