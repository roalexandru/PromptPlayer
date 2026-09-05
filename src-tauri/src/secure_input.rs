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
    #[allow(dead_code)]
    fn IsSecureEventInputEnabled() -> u8;
}

/// Returns true when macOS has Secure Event Input engaged.
/// On non-mac targets always returns false.
///
/// In test builds, always returns false so unit tests aren't sensitive to
/// the developer's terminal-secure-input setting at the time tests run.
#[allow(unused)]
pub fn is_active() -> bool {
    #[cfg(test)]
    {
        false
    }
    #[cfg(all(target_os = "macos", not(test)))]
    unsafe {
        IsSecureEventInputEnabled() != 0
    }
    #[cfg(all(not(target_os = "macos"), not(test)))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_active_returns_false_in_tests() {
        // Sanity check: tests must not be sensitive to the dev's local
        // SecureInput state. The cfg(test) override above guarantees this.
        assert!(!is_active());
    }
}
