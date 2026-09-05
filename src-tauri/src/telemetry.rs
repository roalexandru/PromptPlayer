//! §12 — Aptabase telemetry (`A-EU-9005405380`). Compile-time event whitelist,
//! no opt-out, NEVER prompt content.
//! Four rules from the first release's data: aggregate poller output, keep
//! health signals off the failing path, never restate a column, never ship a
//! constant field.

use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Once;

pub const APTABASE_KEY: &str = "A-EU-9005405380";

/// Every event name we will ever send. `tests/telemetry_contract.rs` fails the
/// build on a variant with no emit site — two sat dead for three releases.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "name", content = "props")]
pub enum TelemetryEvent {
    /// Once per launch. Carries library shape: the only prompt-count signal
    /// used to ride on `CommitObserved`, which needs a live hook AND armed.
    AppStarted {
        prompts: CountBucket,
        triggers: CountBucket,
        hotkeys: CountBucket,
        autostart: bool,
        /// True when the persisted armed state was restored at boot (opt-in).
        armed_restored: bool,
    },
    /// Clean shutdown. Pairs with `AppStarted` for uptime; its absence before
    /// a launch means the previous run died.
    AppExiting {
        uptime: DurationBucket,
        fires: CountBucket,
    },
    /// Health gauge every [`HEARTBEAT_INTERVAL`], from a thread independent of
    /// the hook. Installs run for weeks, so boot-time-only sampling is blind.
    Heartbeat {
        hook_alive: bool,
        accessibility_trusted: bool,
        armed: bool,
        keep_awake: bool,
        prompts: CountBucket,
    },
    /// Boot-time decision pair: did Accessibility pass, and did the tap
    /// install? Separates "trusted but tap failed" (MDM) from plain denial.
    HookInstallResult {
        success: bool,
        accessibility_trusted: bool,
    },
    /// Every later transition of hook liveness — without it a watcher repair
    /// is invisible and we keep reporting the boot-time failure for weeks.
    HookStateChanged {
        alive: bool,
        accessibility_trusted: bool,
        reason: HookChangeReason,
    },
    PromptFired {
        mode: PromptMode,
        char_count_bucket: CharBucket,
        has_expressions: bool,
        target_app_kind: TargetAppKind,
        /// The winning prompt declares a `scope:` filter. Replaces the old
        /// `scope_match`, which restated `target_app_kind` and was always true.
        scoped: bool,
        /// Clipboard-paste delivery rather than synthesized per-key typing.
        paste: bool,
    },
    /// Successful end of a playback. Without it, completion rate was only
    /// inferable by subtraction and duration was not recorded at all.
    PromptCompleted {
        mode: PromptMode,
        char_count_bucket: CharBucket,
        duration: DurationBucket,
        paste: bool,
        /// A backspace-undo entry was armed. Gives `PromptUndone` a
        /// denominator, so zero undos can be told apart from zero offers.
        undo_offered: bool,
    },
    PromptCancelled {
        reason: CancelReason,
        completed_chars_pct: u8,
    },
    /// Panic / kill-switch. `was_playing=false` means nothing was in flight,
    /// which reads very differently from a real abort.
    PromptKilled {
        was_playing: bool,
    },
    PromptUndone,
    PickerOpened {
        source: PickerSource,
    },
    PickerDismissed,
    PickerSearchChars {
        chars_typed: u8,
    },
    /// `hook_alive` rides along because arming a dead hook is a silent no-op —
    /// the user arms, nothing fires, they disarm, and nothing said why.
    ArmToggled {
        armed: bool,
        hook_alive: bool,
    },
    KeepAwakeToggled {
        enabled: bool,
        /// Auto-off duration the user picked, in minutes. `0` = indefinite.
        duration_mins: u16,
    },
    /// The auto-off timer fired — distinguishes "user turned it off" from
    /// "we turned it off for them".
    KeepAwakeExpired,
    ExpressionError {
        kind: ExpressionErrorKind,
    },
    UpdateCheck {
        available: bool,
    },
    /// A check that produced no answer. Without it, a broken updater looks
    /// exactly like a machine that is already up to date.
    UpdateCheckFailed {
        stage: UpdateFailStage,
    },
    /// The "Install update" affordance was rendered. Once per version, so a
    /// long-ignored update doesn't re-count.
    UpdateAvailableShown,
    /// User explicitly dismissed the update affordance.
    UpdateDismissed,
    /// Install handed off. Emitted *and flushed* first because on Windows
    /// `download_and_install` never returns — the installer kills us.
    UpdateInstallStarted,
    /// Install returned successfully (macOS path).
    UpdateApplied {
        /// The version we updated to. `String` because it comes from the
        /// network (`update.version`), not a `env!()` literal.
        to_version: String,
    },
    UpdateInstallFailed,
    /// Aggregated Secure-Input activity for one window. The per-edge version was
    /// 91% of all events and never said whether a trigger was actually lost.
    SecureInputWindow {
        /// How many times the gate closed during the window.
        activations: CountBucket,
        /// Total time the gate spent closed during the window.
        active: DurationBucket,
        /// Commit chars the user typed while the gate was closed — i.e.
        /// triggers that silently did nothing.
        blocked_commits: CountBucket,
    },
    /// A trigger matched but no candidate's `scope:` allowed the foreground app,
    /// so nothing fired. Reads to the user exactly like a broken trigger.
    ScopeRejected {
        candidates: CountBucket,
        target_app_kind: TargetAppKind,
    },
    RdpDetected,
    /// Commit char typed while armed. `matched=false` is every "I expected
    /// this to fire and it didn't" — the key trigger-pipeline metric.
    CommitObserved {
        matched: bool,
        index_size_bucket: CountBucket,
    },
    /// A per-prompt global hotkey fired.
    HotkeyFired,
    /// A per-prompt hotkey failed to register, almost always a conflict.
    /// Previously the hotkey just never worked and nothing recorded it.
    HotkeyRegisterFailed {
        reason: HotkeyFailReason,
    },
    /// Keystroke synthesis or clipboard delivery failed. The clipboard case
    /// silently downgrades paste to per-key typing, which the user notices.
    InjectionFailed {
        stage: InjectionStage,
    },
    /// The diagnostics window was opened.
    DiagnosticsOpened,
    /// User asked us to run `tccutil reset Accessibility` and re-approve.
    AccessibilityReset,
    /// Self test finished. Covers permission → hook → injector → roundtrip.
    SelfTestCompleted {
        passed: bool,
        stage: SelfTestStage,
    },
}

/// How often [`TelemetryEvent::Heartbeat`] is emitted.
pub const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Coarse count bucket — "0 loaded" vs "many" without exact counts. Was
/// `IndexSizeBucket`; wire values unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CountBucket {
    Empty, // 0
    Few,   // 1..5
    Some,  // 5..20
    Many,  // 20+
}

impl CountBucket {
    pub fn classify(n: usize) -> Self {
        match n {
            0 => Self::Empty,
            1..=4 => Self::Few,
            5..=19 => Self::Some,
            _ => Self::Many,
        }
    }
}

/// Coarse duration bucket, from sub-second playbacks to multi-day sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DurationBucket {
    Instant,  // <1s
    Seconds,  // 1s..10s
    UnderMin, // 10s..1m
    Minutes,  // 1m..10m
    UnderHr,  // 10m..1h
    Hours,    // 1h..4h
    HalfDay,  // 4h..12h
    AllDay,   // 12h+
}

impl DurationBucket {
    pub fn classify(d: std::time::Duration) -> Self {
        match d.as_secs() {
            0 => Self::Instant,
            1..=9 => Self::Seconds,
            10..=59 => Self::UnderMin,
            60..=599 => Self::Minutes,
            600..=3599 => Self::UnderHr,
            3600..=14399 => Self::Hours,
            14400..=43199 => Self::HalfDay,
            _ => Self::AllDay,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptMode {
    Stealth,
    Picker,
    Hotkey,
    Tray,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CharBucket {
    Tiny,   // <30
    Small,  // 30..100
    Medium, // 100..500
    Large,  // 500..2000
    Huge,   // >=2000
}

impl CharBucket {
    pub fn classify(n: usize) -> Self {
        match n {
            0..=29 => Self::Tiny,
            30..=99 => Self::Small,
            100..=499 => Self::Medium,
            500..=1999 => Self::Large,
            _ => Self::Huge,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetAppKind {
    Browser,
    Native,
    Rdp,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CancelReason {
    UserKeystrokes,
    Esc,
    Error,
    Kill,
    /// Foreground app changed mid-playback (click / Alt-Tab / notification).
    /// We abort so the remainder isn't typed into the wrong window.
    FocusChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExpressionErrorKind {
    Syntax,
    Runtime,
    Timeout,
}

/// Which entry point summoned the palette. Three copy-pasted open sequences
/// existed and only one emitted anything, so the funnel was invisible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PickerSource {
    Shortcut,
    TrayMenu,
    Ipc,
    Relaunch,
}

/// Why hook liveness changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookChangeReason {
    /// The Accessibility watcher respawned the tap after permission was granted.
    Respawn,
    /// The tap installed and began dispatching.
    Installed,
    /// The tap's run loop exited or the OS disabled it.
    Died,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateFailStage {
    /// `app.updater()` itself was unavailable.
    Unavailable,
    /// The network check / manifest parse failed.
    Check,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HotkeyFailReason {
    /// The string in frontmatter isn't a valid accelerator.
    Unparseable,
    /// The OS refused the registration — combo already claimed.
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InjectionStage {
    /// `EnigoInjector::new()` failed — no synthesis backend at all.
    Init,
    /// Clipboard write / paste failed; we fell back to per-key typing.
    Clipboard,
    /// The literal-commit escape hatch (`\>`) couldn't type.
    LiteralCommit,
    /// The undo backspace run couldn't start.
    Undo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelfTestStage {
    Accessibility,
    Hook,
    Injector,
    SecureInput,
    Library,
    Roundtrip,
}

/// Initialize global telemetry. Must be called once at app startup.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        tracing::info!("telemetry init — aptabase key {}", APTABASE_KEY);
    });
}

/// Send a whitelisted event. The Aptabase plugin batches and buffers offline;
/// §12's no-prompt-content rule is enforced by the enum's shape.
pub fn send(app: &tauri::AppHandle, event: TelemetryEvent) {
    let Some((name, props)) = flatten(&event) else {
        return;
    };
    tracing::debug!("telemetry: {} {}", name, event.short_name());

    // In unit tests the plugin isn't registered, so this drops silently.
    #[cfg(not(test))]
    forward_to_aptabase(app, &name, props);
    #[cfg(test)]
    {
        let _ = (app, props);
    }
}

/// Send and block until the queue drains. Needed wherever the process is about
/// to die — the plugin only auto-flushes on `RunEvent::Exit`.
pub fn send_and_flush(app: &tauri::AppHandle, event: TelemetryEvent) {
    send(app, event);
    flush(app);
}

/// Drain the pending event queue, blocking the caller.
pub fn flush(app: &tauri::AppHandle) {
    #[cfg(not(test))]
    {
        if std::env::var_os("PROMPT_PLAYER_E2E").is_some() {
            return;
        }
        use tauri_plugin_aptabase::EventTracker;
        app.flush_events_blocking();
    }
    #[cfg(test)]
    {
        let _ = app;
    }
}

/// Split a serialized event into `(name, props)`.
fn flatten(event: &TelemetryEvent) -> Option<(String, Value)> {
    let payload = serde_json::to_value(event).unwrap_or(Value::Null);
    let Value::Object(map) = payload else {
        return None;
    };
    let name = map.get("name")?.as_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    let props = map.get("props").cloned().unwrap_or(json!({}));
    Some((name, props))
}

#[cfg(not(test))]
fn forward_to_aptabase(app: &tauri::AppHandle, name: &str, props: Value) {
    // CI/e2e sets PROMPT_PLAYER_E2E=1 so automated launches don't register as
    // real users. Nobody else sets it.
    if std::env::var_os("PROMPT_PLAYER_E2E").is_some() {
        tracing::debug!("telemetry: dropped {} (E2E mode)", name);
        return;
    }
    use tauri_plugin_aptabase::EventTracker;
    let props_opt = match props {
        Value::Object(_)
        | Value::Array(_)
        | Value::String(_)
        | Value::Number(_)
        | Value::Bool(_) => Some(props),
        Value::Null => None,
    };
    if let Err(e) = app.track_event(name, props_opt) {
        tracing::debug!("aptabase track_event failed: {}", e);
    }
}

impl TelemetryEvent {
    pub fn short_name(&self) -> &'static str {
        match self {
            Self::AppStarted { .. } => "app_started",
            Self::AppExiting { .. } => "app_exiting",
            Self::Heartbeat { .. } => "heartbeat",
            Self::HookInstallResult { .. } => "hook_install_result",
            Self::HookStateChanged { .. } => "hook_state_changed",
            Self::PromptFired { .. } => "prompt_fired",
            Self::PromptCompleted { .. } => "prompt_completed",
            Self::PromptCancelled { .. } => "prompt_cancelled",
            Self::PromptKilled { .. } => "prompt_killed",
            Self::PromptUndone => "prompt_undone",
            Self::PickerOpened { .. } => "picker_opened",
            Self::PickerDismissed => "picker_dismissed",
            Self::PickerSearchChars { .. } => "picker_search_chars",
            Self::ArmToggled { .. } => "arm_toggled",
            Self::KeepAwakeToggled { .. } => "keep_awake_toggled",
            Self::KeepAwakeExpired => "keep_awake_expired",
            Self::ExpressionError { .. } => "expression_error",
            Self::UpdateCheck { .. } => "update_check",
            Self::UpdateCheckFailed { .. } => "update_check_failed",
            Self::UpdateAvailableShown => "update_available_shown",
            Self::UpdateDismissed => "update_dismissed",
            Self::UpdateInstallStarted => "update_install_started",
            Self::UpdateApplied { .. } => "update_applied",
            Self::UpdateInstallFailed => "update_install_failed",
            Self::SecureInputWindow { .. } => "secure_input_window",
            Self::ScopeRejected { .. } => "scope_rejected",
            Self::RdpDetected => "rdp_detected",
            Self::CommitObserved { .. } => "commit_observed",
            Self::HotkeyFired => "hotkey_fired",
            Self::HotkeyRegisterFailed { .. } => "hotkey_register_failed",
            Self::InjectionFailed { .. } => "injection_failed",
            Self::DiagnosticsOpened => "diagnostics_opened",
            Self::AccessibilityReset => "accessibility_reset",
            Self::SelfTestCompleted { .. } => "self_test_completed",
        }
    }

    /// The wire name Aptabase sees (the serde tag). Used by the contract test.
    pub fn wire_name(&self) -> String {
        flatten(self).map(|(n, _)| n).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One instance of every variant; kept exhaustive by the wildcard-free
    /// `match` in `variant_coverage_is_exhaustive`.
    pub(crate) fn all_variants() -> Vec<TelemetryEvent> {
        vec![
            TelemetryEvent::AppStarted {
                prompts: CountBucket::Few,
                triggers: CountBucket::Some,
                hotkeys: CountBucket::Empty,
                autostart: true,
                armed_restored: false,
            },
            TelemetryEvent::AppExiting {
                uptime: DurationBucket::Hours,
                fires: CountBucket::Few,
            },
            TelemetryEvent::Heartbeat {
                hook_alive: true,
                accessibility_trusted: true,
                armed: false,
                keep_awake: false,
                prompts: CountBucket::Few,
            },
            TelemetryEvent::HookInstallResult {
                success: true,
                accessibility_trusted: true,
            },
            TelemetryEvent::HookStateChanged {
                alive: true,
                accessibility_trusted: true,
                reason: HookChangeReason::Respawn,
            },
            TelemetryEvent::PromptFired {
                mode: PromptMode::Stealth,
                char_count_bucket: CharBucket::Medium,
                has_expressions: false,
                target_app_kind: TargetAppKind::Native,
                scoped: true,
                paste: false,
            },
            TelemetryEvent::PromptCompleted {
                mode: PromptMode::Picker,
                char_count_bucket: CharBucket::Small,
                duration: DurationBucket::Seconds,
                paste: true,
                undo_offered: true,
            },
            TelemetryEvent::PromptCancelled {
                reason: CancelReason::UserKeystrokes,
                completed_chars_pct: 23,
            },
            TelemetryEvent::PromptKilled { was_playing: true },
            TelemetryEvent::PromptUndone,
            TelemetryEvent::PickerOpened {
                source: PickerSource::TrayMenu,
            },
            TelemetryEvent::PickerDismissed,
            TelemetryEvent::PickerSearchChars { chars_typed: 4 },
            TelemetryEvent::ArmToggled {
                armed: true,
                hook_alive: false,
            },
            TelemetryEvent::KeepAwakeToggled {
                enabled: true,
                duration_mins: 60,
            },
            TelemetryEvent::KeepAwakeExpired,
            TelemetryEvent::ExpressionError {
                kind: ExpressionErrorKind::Timeout,
            },
            TelemetryEvent::UpdateCheck { available: false },
            TelemetryEvent::UpdateCheckFailed {
                stage: UpdateFailStage::Check,
            },
            TelemetryEvent::UpdateAvailableShown,
            TelemetryEvent::UpdateDismissed,
            TelemetryEvent::UpdateInstallStarted,
            TelemetryEvent::UpdateApplied {
                to_version: "0.1.9".into(),
            },
            TelemetryEvent::UpdateInstallFailed,
            TelemetryEvent::SecureInputWindow {
                activations: CountBucket::Many,
                active: DurationBucket::Minutes,
                blocked_commits: CountBucket::Empty,
            },
            TelemetryEvent::ScopeRejected {
                candidates: CountBucket::Few,
                target_app_kind: TargetAppKind::Browser,
            },
            TelemetryEvent::RdpDetected,
            TelemetryEvent::CommitObserved {
                matched: false,
                index_size_bucket: CountBucket::Few,
            },
            TelemetryEvent::HotkeyFired,
            TelemetryEvent::HotkeyRegisterFailed {
                reason: HotkeyFailReason::Conflict,
            },
            TelemetryEvent::InjectionFailed {
                stage: InjectionStage::Clipboard,
            },
            TelemetryEvent::DiagnosticsOpened,
            TelemetryEvent::AccessibilityReset,
            TelemetryEvent::SelfTestCompleted {
                passed: true,
                stage: SelfTestStage::Roundtrip,
            },
        ]
    }

    /// Wildcard-free `match`: a new variant breaks the build until it is added
    /// to `all_variants()`, which the payload and emit-site tests iterate.
    #[test]
    fn variant_coverage_is_exhaustive() {
        for e in all_variants() {
            let counted = match e {
                TelemetryEvent::AppStarted { .. }
                | TelemetryEvent::AppExiting { .. }
                | TelemetryEvent::Heartbeat { .. }
                | TelemetryEvent::HookInstallResult { .. }
                | TelemetryEvent::HookStateChanged { .. }
                | TelemetryEvent::PromptFired { .. }
                | TelemetryEvent::PromptCompleted { .. }
                | TelemetryEvent::PromptCancelled { .. }
                | TelemetryEvent::PromptKilled { .. }
                | TelemetryEvent::PromptUndone
                | TelemetryEvent::PickerOpened { .. }
                | TelemetryEvent::PickerDismissed
                | TelemetryEvent::PickerSearchChars { .. }
                | TelemetryEvent::ArmToggled { .. }
                | TelemetryEvent::KeepAwakeToggled { .. }
                | TelemetryEvent::KeepAwakeExpired
                | TelemetryEvent::ExpressionError { .. }
                | TelemetryEvent::UpdateCheck { .. }
                | TelemetryEvent::UpdateCheckFailed { .. }
                | TelemetryEvent::UpdateAvailableShown
                | TelemetryEvent::UpdateDismissed
                | TelemetryEvent::UpdateInstallStarted
                | TelemetryEvent::UpdateApplied { .. }
                | TelemetryEvent::UpdateInstallFailed
                | TelemetryEvent::SecureInputWindow { .. }
                | TelemetryEvent::ScopeRejected { .. }
                | TelemetryEvent::RdpDetected
                | TelemetryEvent::CommitObserved { .. }
                | TelemetryEvent::HotkeyFired
                | TelemetryEvent::HotkeyRegisterFailed { .. }
                | TelemetryEvent::InjectionFailed { .. }
                | TelemetryEvent::DiagnosticsOpened
                | TelemetryEvent::AccessibilityReset
                | TelemetryEvent::SelfTestCompleted { .. } => 1,
            };
            assert_eq!(counted, 1);
        }
    }

    /// §12 — no event payload may carry a free-text field that could leak
    /// prompt content. Every props value must be bool / number / short enum.
    #[test]
    fn no_event_payload_includes_long_strings() {
        for e in all_variants() {
            let v = serde_json::to_value(&e).unwrap();
            check_no_long_strings(&v);
        }
    }

    fn check_no_long_strings(v: &Value) {
        match v {
            Value::String(s) => assert!(
                s.len() < 32,
                "event payload contains a string longer than 32 chars: {:?}",
                s
            ),
            Value::Array(a) => a.iter().for_each(check_no_long_strings),
            Value::Object(o) => o.values().for_each(check_no_long_strings),
            _ => {}
        }
    }

    /// Distinct wire and short names — a copy-paste slip would otherwise
    /// silently merge two events.
    #[test]
    fn names_are_unique_and_non_empty() {
        let mut wire = std::collections::HashSet::new();
        let mut short = std::collections::HashSet::new();
        for e in all_variants() {
            let w = e.wire_name();
            assert!(!w.is_empty(), "empty wire name for {:?}", e);
            assert!(wire.insert(w.clone()), "duplicate wire name {}", w);
            let s = e.short_name();
            assert!(short.insert(s), "duplicate short_name {}", s);
        }
    }

    /// No props key may restate a column Aptabase already sends — that is how
    /// `AppStarted` carried `os`/`locale`/`version` and nothing about prompts.
    #[test]
    fn no_payload_duplicates_an_aptabase_column() {
        const RESERVED: &[&str] = &[
            "os",
            "os_name",
            "os_version",
            "locale",
            "version",
            "app_version",
            "app_build_number",
            "engine_name",
            "engine_version",
            "country_code",
            "region_name",
            "user_id",
            "session_id",
            "timestamp",
        ];
        for e in all_variants() {
            let Some((name, props)) = flatten(&e) else {
                continue;
            };
            if let Value::Object(map) = props {
                for key in map.keys() {
                    assert!(
                        !RESERVED.contains(&key.as_str()),
                        "{} carries `{}`, which Aptabase already sends as a column",
                        name,
                        key
                    );
                }
            }
        }
    }

    #[test]
    fn char_bucket_classify() {
        assert!(matches!(CharBucket::classify(10), CharBucket::Tiny));
        assert!(matches!(CharBucket::classify(50), CharBucket::Small));
        assert!(matches!(CharBucket::classify(200), CharBucket::Medium));
        assert!(matches!(CharBucket::classify(1000), CharBucket::Large));
        assert!(matches!(CharBucket::classify(5000), CharBucket::Huge));
    }

    #[test]
    fn count_bucket_classify() {
        assert_eq!(CountBucket::classify(0), CountBucket::Empty);
        assert_eq!(CountBucket::classify(1), CountBucket::Few);
        assert_eq!(CountBucket::classify(4), CountBucket::Few);
        assert_eq!(CountBucket::classify(5), CountBucket::Some);
        assert_eq!(CountBucket::classify(19), CountBucket::Some);
        assert_eq!(CountBucket::classify(20), CountBucket::Many);
    }

    #[test]
    fn duration_bucket_classify() {
        use std::time::Duration;
        assert_eq!(
            DurationBucket::classify(Duration::from_millis(400)),
            DurationBucket::Instant
        );
        assert_eq!(
            DurationBucket::classify(Duration::from_secs(3)),
            DurationBucket::Seconds
        );
        assert_eq!(
            DurationBucket::classify(Duration::from_secs(30)),
            DurationBucket::UnderMin
        );
        assert_eq!(
            DurationBucket::classify(Duration::from_secs(300)),
            DurationBucket::Minutes
        );
        assert_eq!(
            DurationBucket::classify(Duration::from_secs(1800)),
            DurationBucket::UnderHr
        );
        assert_eq!(
            DurationBucket::classify(Duration::from_secs(2 * 3600)),
            DurationBucket::Hours
        );
        assert_eq!(
            DurationBucket::classify(Duration::from_secs(6 * 3600)),
            DurationBucket::HalfDay
        );
        // The 3d14h keep-awake session seen in the field.
        assert_eq!(
            DurationBucket::classify(Duration::from_secs(86_400 * 3)),
            DurationBucket::AllDay
        );
    }
}
