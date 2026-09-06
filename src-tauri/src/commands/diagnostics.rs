//! Diagnostics / first-run setup IPC. Exists because the field data showed
//! macOS installs running for weeks with a dead keyboard hook and zero fires,
//! while nothing told the user or let them repair it.

use crate::app::context::AppContext;
use crate::settings::UiSettings;
use crate::telemetry::{self, SelfTestStage, TelemetryEvent};
use tauri::{AppHandle, Manager};

/// Everything the diagnostics window shows. One round trip, so the UI can't
/// render a half-stale picture of a machine that is mid-repair.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub version: String,
    pub os: String,
    /// macOS Accessibility (TCC). Always true where the concept doesn't apply.
    pub accessibility_trusted: bool,
    /// The keyboard hook is installed and dispatching.
    pub hook_alive: bool,
    /// macOS Secure Input is engaged right now, so triggers are gated off.
    pub secure_input_active: bool,
    /// §5.4 exclusion is not fully in effect — the picker may show up in a
    /// screen share. Sticky until the next successful show.
    pub capture_degraded: bool,
    pub armed: bool,
    pub keep_awake: bool,
    pub prompts: u32,
    pub enabled_prompts: u32,
    pub triggers: u32,
    pub hotkeys: u32,
    pub library_root: String,
    pub log_dir: String,
    /// True when the hook cannot work and the user must act.
    pub needs_attention: bool,
}

#[tauri::command]
#[specta::specta]
pub fn get_diagnostics(ctx: tauri::State<'_, AppContext>) -> Diagnostics {
    collect(&ctx)
}

pub fn collect(ctx: &AppContext) -> Diagnostics {
    let prompts = ctx.prompts.snapshot();
    let enabled: Vec<_> = prompts.iter().filter(|p| p.enabled).collect();
    let triggers: usize = enabled.iter().map(|p| p.triggers.len()).sum();
    let accessibility_trusted = crate::tcc::is_accessibility_trusted();
    let hook_alive = ctx.state.hook_alive();
    Diagnostics {
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        accessibility_trusted,
        hook_alive,
        secure_input_active: crate::secure_input::is_active(),
        capture_degraded: ctx.attention.capture_degraded(),
        armed: ctx.state.is_armed(),
        keep_awake: ctx.power.is_enabled(),
        prompts: prompts.len() as u32,
        enabled_prompts: enabled.len() as u32,
        triggers: triggers as u32,
        hotkeys: ctx.hotkeys.read().len() as u32,
        library_root: crate::prompts::library::default_library_root()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        log_dir: log_dir(),
        needs_attention: !hook_alive || !accessibility_trusted,
    }
}

fn log_dir() -> String {
    dirs::data_local_dir()
        .map(|d| d.join("PromptPlayer").join("logs").display().to_string())
        .unwrap_or_default()
}

/// One self-test step and whether it passed.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SelfTestStep {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

/// Result of the pre-flight self test.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SelfTestReport {
    pub passed: bool,
    pub steps: Vec<SelfTestStep>,
    /// The string `self_test_type` will inject, for the UI to compare against.
    pub probe: String,
}

/// The string the roundtrip step types. Deliberately boring ASCII so no
/// keyboard layout can mangle it.
pub const SELF_TEST_PROBE: &str = "prompt player ok";

/// Check everything between a keypress and a typed character, in order, and
/// stop at the first failure so the UI can name one actionable thing.
#[tauri::command]
#[specta::specta]
pub fn run_self_test(app: AppHandle, ctx: tauri::State<'_, AppContext>) -> SelfTestReport {
    let d = collect(&ctx);
    let mut steps = Vec::new();
    let mut first_failure = None;

    let mut step = |name: &str, passed: bool, detail: &str, stage: SelfTestStage| {
        steps.push(SelfTestStep {
            name: name.to_string(),
            passed,
            detail: detail.to_string(),
        });
        if !passed && first_failure.is_none() {
            first_failure = Some(stage);
        }
    };

    step(
        "Accessibility permission",
        d.accessibility_trusted,
        if d.accessibility_trusted {
            "Granted"
        } else {
            "Not granted — the keyboard hook cannot install"
        },
        SelfTestStage::Accessibility,
    );
    step(
        "Keyboard hook",
        d.hook_alive,
        if d.hook_alive {
            "Installed and listening"
        } else {
            "Not installed — triggers will never fire"
        },
        SelfTestStage::Hook,
    );
    step(
        "Secure Input",
        !d.secure_input_active,
        if d.secure_input_active {
            "Engaged right now — triggers are gated off"
        } else {
            "Clear"
        },
        SelfTestStage::SecureInput,
    );
    let injector_ok = crate::inject::EnigoInjector::new().is_ok();
    step(
        "Keystroke synthesis",
        injector_ok,
        if injector_ok {
            "Injector ready"
        } else {
            "Could not create an injector — nothing can be typed"
        },
        SelfTestStage::Injector,
    );
    let library_ok = d.triggers > 0;
    step(
        "Prompt library",
        library_ok,
        if library_ok {
            "Triggers indexed"
        } else {
            "No enabled triggers — nothing to match"
        },
        SelfTestStage::Library,
    );

    let passed = steps.iter().all(|s| s.passed);
    telemetry::send(
        &app,
        TelemetryEvent::SelfTestCompleted {
            passed,
            stage: first_failure.unwrap_or(SelfTestStage::Roundtrip),
        },
    );
    SelfTestReport {
        passed,
        steps,
        probe: SELF_TEST_PROBE.to_string(),
    }
}

/// Type [`SELF_TEST_PROBE`] into whatever has focus. The window focuses its own
/// field first, making this a real end-to-end check rather than a status read.
#[tauri::command]
#[specta::specta]
pub fn self_test_type() {
    std::thread::Builder::new()
        .name("prompt-player-self-test".into())
        .spawn(|| {
            use crate::typer::Injector;
            match crate::inject::EnigoInjector::new() {
                Ok(mut inj) => {
                    for c in SELF_TEST_PROBE.chars() {
                        inj.type_char(c);
                        std::thread::sleep(std::time::Duration::from_millis(12));
                    }
                }
                Err(e) => tracing::error!("self-test injector init failed: {:?}", e),
            }
        })
        .expect("spawn self-test thread");
}

/// Show the diagnostics window, creating nothing — it's declared in
/// `tauri.conf.json` like the other secondary windows.
#[tauri::command]
#[specta::specta]
pub fn open_diagnostics(app: AppHandle, ctx: tauri::State<'_, AppContext>) {
    ctx.settings.update(|s| s.setup_seen = true);
    telemetry::send(&app, TelemetryEvent::DiagnosticsOpened);
    show(&app);
}

pub fn show(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("diagnostics") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        #[cfg(target_os = "macos")]
        crate::platform::macos::activate_app();
        // The status poll only runs while this window is on screen.
        crate::app::lifecycle::notify_shown(app, "diagnostics");
    }
}

/// Read the persisted settings the UI can edit.
#[tauri::command]
#[specta::specta]
pub fn get_settings(ctx: tauri::State<'_, AppContext>) -> UiSettings {
    UiSettings::from(&ctx.settings.get())
}

/// Opt in/out of restoring `armed` at launch. §10.1 stays the default.
#[tauri::command]
#[specta::specta]
pub fn set_restore_armed(restore: bool, ctx: tauri::State<'_, AppContext>) -> UiSettings {
    let s = ctx.settings.update(|s| {
        s.restore_armed = restore;
        if restore {
            s.armed = ctx.state.is_armed();
        }
    });
    UiSettings::from(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_is_layout_safe_and_short() {
        // Injected char-by-char through the OS, so anything but lowercase
        // ASCII risks a dead-key or layout surprise on a non-US keyboard.
        assert!(SELF_TEST_PROBE
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == ' '));
        assert!(!SELF_TEST_PROBE.is_empty());
        assert!(SELF_TEST_PROBE.len() < 32);
    }
}
