//! NSWorkspace bridge — the one place answering "which app is foreground" and
//! "activate this pid". Uses type-safe `objc2-app-kit` rather than `msg_send!`,
//! the pattern new macOS platform code should follow.

use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy,
    NSRunningApplication, NSWorkspace,
};
use objc2_foundation::MainThreadMarker;

#[derive(Debug, Clone, Default)]
pub struct FrontmostApp {
    pub bundle_id: Option<String>,
    pub executable_path: Option<String>,
    pub pid: Option<i32>,
}

/// Snapshot the current frontmost app via `NSWorkspace.frontmostApplication`.
/// Returns an all-`None` snapshot if no app is frontmost.
pub fn frontmost_app() -> FrontmostApp {
    // SAFETY: NSWorkspace.sharedWorkspace and frontmostApplication are
    // documented as thread-safe for read access.
    unsafe {
        let workspace: Retained<NSWorkspace> = NSWorkspace::sharedWorkspace();
        let Some(app) = workspace.frontmostApplication() else {
            return FrontmostApp::default();
        };
        FrontmostApp {
            bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
            executable_path: app
                .executableURL()
                .and_then(|url| url.path())
                .map(|s| s.to_string()),
            pid: Some(app.processIdentifier()),
        }
    }
}

/// Activate the app identified by PID (re-bring to foreground).
pub fn activate_pid(pid: i32) -> bool {
    // SAFETY: runningApplicationWithProcessIdentifier and activateWithOptions
    // are thread-safe per Apple's NSRunningApplication docs.
    unsafe {
        let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
            return false;
        };
        app.activateWithOptions(
            NSApplicationActivationOptions::NSApplicationActivateIgnoringOtherApps,
        )
    }
}

/// Set the `.accessory` activation policy — no Dock icon, no Cmd+Tab entry.
/// Call once at startup.
pub fn set_accessory_activation_policy() {
    // SAFETY: Tauri's setup hook runs on the main thread.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
}
