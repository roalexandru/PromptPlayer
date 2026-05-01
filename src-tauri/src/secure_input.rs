//! §9.1 — macOS Secure Input detection.
//!
//! When password fields are focused (1Password, Keychain, sudo in Terminal),
//! macOS engages Secure Event Input which blocks `CGEventTap` from suppressing
//! keystrokes. We detect via `IsSecureEventInputEnabled()` and:
//!  - Disable trigger detection while active (passes everything through).
//!  - Show tray icon "🔒 Secure Input Active" indicator (Phase 13).
//!  - Log telemetry event (no content, Phase 10).

#[cfg(target_os = "macos")]
extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

/// Returns true when macOS has Secure Event Input engaged.
/// On non-mac targets always returns false.
#[allow(unused)]
pub fn is_active() -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        IsSecureEventInputEnabled() != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}
