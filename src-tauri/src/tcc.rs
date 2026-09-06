//! §9.1 — macOS Accessibility (TCC). `reset_accessibility` fixes the
//! "approved but not working" state an unsigned upgrade leaves behind, and is
//! surfaced by the diagnostics window's "Reset & Reapprove".

/// Bundle id, locked (CI asserts it) — see `tauri.conf.json`.
pub const BUNDLE_ID: &str = "com.roalexandru.promptplayer";

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

/// First-launch probe: prompting registers us in the Accessibility list so the
/// pane has a toggle. Idempotent, and silent when already trusted.
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
pub fn reset_accessibility(bundle_id: &str) -> bool {
    match std::process::Command::new("tccutil")
        .args(["reset", "Accessibility", bundle_id])
        .status()
    {
        Ok(status) if status.success() => true,
        Ok(status) => {
            tracing::warn!("tccutil reset Accessibility exited {}", status);
            false
        }
        Err(e) => {
            tracing::warn!("tccutil reset Accessibility failed: {}", e);
            false
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn reset_accessibility(_bundle_id: &str) -> bool {
    false
}

/// Open the System Settings → Privacy & Security → Accessibility pane.
#[cfg(target_os = "macos")]
pub fn open_accessibility_settings() {
    // `spawn`, not `status`: this is called from a synchronous Tauri command,
    // which runs on the main thread, and waiting on `open` froze the UI for as
    // long as System Settings took to come up.
    if let Err(e) = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn()
    {
        tracing::warn!("could not open the Accessibility pane: {e}");
    }
}

#[cfg(not(target_os = "macos"))]
pub fn open_accessibility_settings() {}
