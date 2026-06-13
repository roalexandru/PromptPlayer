//! §8.4 — keystroke synthesis. Cross-platform via `enigo` crate.
//!
//! Per §9.4, non-ASCII chars use Unicode injection where the platform supports it
//! (`CGEventCreateKeyboardEvent` + `setUnicodeString` on Mac, `KEYEVENTF_UNICODE`
//! on Win). enigo handles both transparently; longer non-ASCII runs fall back to
//! clipboard paste in Phase 9 (RDP mode disables that fallback).
//!
//! Paste mode (`paste_via_clipboard`) takes a different path entirely: save the
//! clipboard, set it to the body, synthesize Ctrl/Cmd+V once, restore the
//! clipboard. That sidesteps the per-char drop modes (stuck Alt during the
//! picker hand-off, IME coalescing, focus-race against the first chars, surrogate
//! pair splitting) that bite a per-key SendInput stream with no inter-key cadence.

use crate::typer::Injector;
use enigo::{Direction, Enigo, Key as EnigoKey, Keyboard, Settings};

/// Result of a clipboard-paste delivery. Surfaces failures to the caller so it
/// can fall back to per-char injection (RDP, clipboard locked by another app,
/// etc.) without silently dropping the prompt.
#[derive(Debug)]
pub enum PasteError {
    Clipboard(String),
    Injection(String),
}

/// Save the current clipboard, set it to `body`, synthesize Ctrl/Cmd+V, then
/// restore the clipboard. Synchronous: returns only after the paste keystroke
/// has been dispatched AND the original clipboard contents are back.
///
/// Focus must already be on the target window — paste sends the keystroke to
/// whoever is foreground. The caller is responsible for that (see
/// `picker::FocusStore::restore_and_wait`).
pub fn paste_via_clipboard(body: &str) -> Result<(), PasteError> {
    #[cfg(target_os = "windows")]
    {
        windows::paste_via_clipboard(body)
    }
    #[cfg(target_os = "macos")]
    {
        macos::paste_via_clipboard(body)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = body;
        Err(PasteError::Injection(
            "paste_via_clipboard not implemented on this platform".into(),
        ))
    }
}

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

/// Read the system clipboard's plain text, if any. Returns `None` when the
/// clipboard is empty, holds no text flavor, or can't be read. Used to fill
/// the `$CLIPBOARD` placeholder / `clipboard` expression builtin at fire time.
pub fn read_clipboard_text() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::read_clipboard_string()
    }
    #[cfg(target_os = "windows")]
    {
        windows::read_clipboard_string()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

pub struct EnigoInjector {
    enigo: Enigo,
}

impl EnigoInjector {
    pub fn new() -> Result<Self, enigo::NewConError> {
        let enigo = Enigo::new(&Settings::default())?;
        Ok(Self { enigo })
    }
}

impl Injector for EnigoInjector {
    fn type_char(&mut self, c: char) {
        // macOS: enigo's `text()` uses `CGEventCreateKeyboardEvent` +
        // `setUnicodeString`, which works correctly.
        // Windows: enigo 0.2 mis-synthesizes single-char `text()` calls
        // (every ASCII char comes out as 'a'). Use direct `SendInput` with
        // `KEYEVENTF_UNICODE` instead — see `windows::type_char_unicode`.
        #[cfg(target_os = "macos")]
        {
            let s = c.to_string();
            let _ = self.enigo.text(&s);
        }
        #[cfg(target_os = "windows")]
        {
            windows::type_char_unicode(c);
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let s = c.to_string();
            let _ = self.enigo.text(&s);
        }
    }
    fn press_backspace(&mut self) {
        let _ = self.enigo.key(EnigoKey::Backspace, Direction::Click);
    }
    fn press_enter(&mut self) {
        let _ = self.enigo.key(EnigoKey::Return, Direction::Click);
    }
    fn press_shift_enter(&mut self) {
        // Hold Shift across the Return so chat apps insert a line break
        // instead of sending. Release Shift even if the Return errors so we
        // never leave the modifier stuck (§2.7).
        let _ = self.enigo.key(EnigoKey::Shift, Direction::Press);
        let _ = self.enigo.key(EnigoKey::Return, Direction::Click);
        let _ = self.enigo.key(EnigoKey::Shift, Direction::Release);
    }
    fn release_all_modifiers(&mut self) {
        // Defensive: release the common modifiers that could have been captured.
        for k in [
            EnigoKey::Shift,
            EnigoKey::LShift,
            EnigoKey::RShift,
            EnigoKey::Control,
            EnigoKey::LControl,
            EnigoKey::RControl,
            EnigoKey::Alt,
            EnigoKey::Meta,
        ] {
            let _ = self.enigo.key(k, Direction::Release);
        }
    }
}
