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
use objc2_foundation::{NSArray, NSData, NSString};
use std::time::Duration;

const KC_V: CGKeyCode = 9; // ANSI_V

pub(super) fn paste_via_clipboard(body: &str) -> Result<(), PasteError> {
    // 1. Snapshot current pasteboard formats.
    let saved = snapshot_pasteboard();

    // 2. Set ours.
    if let Err(e) = write_pasteboard_string(body) {
        restore_pasteboard(&saved);
        return Err(PasteError::Clipboard(e));
    }

    // 3. Synthesize Cmd+V.
    if let Err(e) = synth_cmd_v() {
        restore_pasteboard(&saved);
        return Err(PasteError::Injection(e));
    }

    // 4. Let the paste land before we overwrite the pasteboard. The Cmd+V
    //    keystroke is delivered asynchronously; Electron/browser chat apps
    //    (the main target) can read the pasteboard well after the keystroke
    //    under load. Restoring too early makes them paste the user's PREVIOUS
    //    clipboard — potentially private content, live on stage. 250ms is
    //    imperceptible (the text is already on screen) and covers slow
    //    consumers; with playbacks mutually exclusive nothing else needs the
    //    fire thread during this window.
    std::thread::sleep(Duration::from_millis(250));

    // 5. Restore every pasteboard flavor we could snapshot.
    if !restore_pasteboard(&saved) {
        tracing::warn!("pasteboard restore failed");
    }
    Ok(())
}

struct PasteboardSnapshot {
    entries: Vec<(Retained<objc2_app_kit::NSPasteboardType>, Vec<u8>)>,
}

fn snapshot_pasteboard() -> PasteboardSnapshot {
    unsafe {
        let pb: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
        let Some(types) = pb.types() else {
            return PasteboardSnapshot {
                entries: Vec::new(),
            };
        };
        let mut entries = Vec::new();
        for i in 0..types.count() {
            let ty = types.objectAtIndex(i);
            if let Some(data) = pb.dataForType(&ty) {
                entries.push((ty, data.bytes().to_vec()));
            }
        }
        PasteboardSnapshot { entries }
    }
}

fn restore_pasteboard(snapshot: &PasteboardSnapshot) -> bool {
    unsafe {
        let pb: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
        let _ = pb.clearContents();
        if snapshot.entries.is_empty() {
            return true;
        }
        let types: Vec<Retained<objc2_app_kit::NSPasteboardType>> =
            snapshot.entries.iter().map(|(ty, _)| ty.clone()).collect();
        let type_array = NSArray::from_vec(types);
        let _ = pb.declareTypes_owner(&type_array, None);
        snapshot.entries.iter().all(|(ty, bytes)| {
            let data = NSData::with_bytes(bytes);
            pb.setData_forType(Some(&data), ty)
        })
    }
}

/// Read the current pasteboard's plain-text flavor, if any. Used to populate
/// the `$CLIPBOARD` placeholder / `clipboard` expression builtin at fire time.
pub(crate) fn read_clipboard_string() -> Option<String> {
    unsafe {
        let pb: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
        pb.stringForType(NSPasteboardTypeString)
            .map(|s| s.to_string())
    }
}

fn write_pasteboard_string(text: &str) -> Result<(), String> {
    unsafe {
        let pb: Retained<NSPasteboard> = NSPasteboard::generalPasteboard();
        // `clearContents` returns the new change count; we don't track it.
        let _ = pb.clearContents();
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
