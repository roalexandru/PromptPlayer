//! §8.4 — keystroke synthesis. Cross-platform via `enigo` crate.
//!
//! Per §9.4, non-ASCII chars use Unicode injection where the platform supports it
//! (`CGEventCreateKeyboardEvent` + `setUnicodeString` on Mac, `KEYEVENTF_UNICODE`
//! on Win). enigo handles both transparently; longer non-ASCII runs fall back to
//! clipboard paste in Phase 9 (RDP mode disables that fallback).

use crate::typer::Injector;
use enigo::{Direction, Enigo, Key as EnigoKey, Keyboard, Settings};

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

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
        let s = c.to_string();
        let _ = self.enigo.text(&s);
    }
    fn press_backspace(&mut self) {
        let _ = self.enigo.key(EnigoKey::Backspace, Direction::Click);
    }
    fn press_enter(&mut self) {
        let _ = self.enigo.key(EnigoKey::Return, Direction::Click);
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
