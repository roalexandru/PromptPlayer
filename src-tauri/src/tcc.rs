//! §9.1 — macOS TCC reset utility.
//!
//! When Accessibility permission gets stuck in the "approved but not working"
//! state (common after upgrades or signing changes), surface a one-click
//! "Reset & Reapprove" that runs:
//!     tccutil reset Accessibility com.roalexandru.promptplayer
//! and walks the user back through the System Settings approval flow.

#[cfg(target_os = "macos")]
fn ax_is_trusted(prompt: bool) -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
    }
    unsafe {
        let key = CFString::from_static_string("AXTrustedCheckOptionPrompt");
        let val = if prompt {
            core_foundation::boolean::CFBoolean::true_value()
        } else {
            core_foundation::boolean::CFBoolean::false_value()
        };
        let dict = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), val.as_CFType())]);
        AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef() as *const _)
    }
}

/// Read-only check — does NOT add the app to the Accessibility list and does NOT
/// show the system prompt. Use this from the polling watcher.
#[cfg(target_os = "macos")]
pub fn is_accessibility_trusted() -> bool {
    ax_is_trusted(false)
}

/// First-launch probe. Setting `prompt=true` makes macOS register this app in
/// the Accessibility list (so the System Settings pane has something to toggle)
/// and surfaces the system permission prompt. Idempotent — safe to call on every
/// launch; if already trusted it just returns true with no UI.
#[cfg(target_os = "macos")]
pub fn prompt_for_accessibility() -> bool {
    ax_is_trusted(true)
}

#[cfg(not(target_os = "macos"))]
pub fn is_accessibility_trusted() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn prompt_for_accessibility() -> bool {
    true
}

/// Run `tccutil reset Accessibility <bundle_id>`. macOS only.
/// Returns the exit status of the subprocess.
#[cfg(target_os = "macos")]
pub fn reset_accessibility(bundle_id: &str) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new("tccutil")
        .args(["reset", "Accessibility", bundle_id])
        .status()
}

#[cfg(not(target_os = "macos"))]
pub fn reset_accessibility(_bundle_id: &str) -> std::io::Result<std::process::ExitStatus> {
    use std::os::process::ExitStatusExt;
    Ok(std::process::ExitStatus::from_raw(0))
}

/// Open the System Settings → Privacy & Security → Accessibility pane.
#[cfg(target_os = "macos")]
pub fn open_accessibility_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status();
}

#[cfg(not(target_os = "macos"))]
pub fn open_accessibility_settings() {}
