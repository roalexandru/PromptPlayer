//! §8.4 — keystroke synthesis via `enigo`, with Unicode injection for non-ASCII
//! (§9.4) and a clipboard fallback for long runs, disabled under RDP.
//!
//! `paste_via_clipboard` sidesteps the per-char drop modes — stuck Alt, IME
//! coalescing, focus races, split surrogates — with a single Ctrl/Cmd+V.

use crate::typer::Injector;
use enigo::{Direction, Enigo, Key as EnigoKey, Keyboard, Settings};

/// Clipboard-paste outcome. Failures surface so the caller can fall back to
/// per-char injection instead of silently dropping the prompt.
#[derive(Debug)]
pub enum PasteError {
    Clipboard(String),
    Injection(String),
}

/// Save the clipboard, set `body`, Ctrl/Cmd+V, restore. The caller must already
/// hold focus on the target — paste goes to whoever is foreground.
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

/// Plain text from the clipboard, or `None` if empty/unreadable. Fills the
/// `$CLIPBOARD` placeholder and the `clipboard` builtin at fire time.
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
        // enigo's `text()` is correct on macOS but mis-synthesizes single chars
        // on Windows, so that path uses `SendInput` directly.
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
        // Shift+Return inserts a line break instead of sending. Released even on
        // error so the modifier never sticks (§2.7).
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
