//! §12 — Telemetry via Aptabase.
//!
//! - Per Q5: minimum viable, no opt-out, debug vs prod separation, NO PROMPT CONTENT.
//! - Whitelist of event names enforced at compile time (TelemetryEvent enum).
//! - Always-on in prod; toggleable in debug builds only.
//! - Aptabase project key: `A-EU-9005405380` (used for both debug and prod for now).

use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Once;

pub const APTABASE_KEY: &str = "A-EU-9005405380";

/// The complete set of event names we will EVER send. Adding a new variant
/// requires explicit code review.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "name", content = "props")]
pub enum TelemetryEvent {
    AppStarted {
        version: &'static str,
        os: &'static str,
        locale: String,
        profile_in_use: &'static str,
    },
    PromptFired {
        mode: PromptMode,
        char_count_bucket: CharBucket,
        has_expressions: bool,
        target_app_kind: TargetAppKind,
        scope_match: bool,
    },
    PromptCancelled {
        reason: CancelReason,
        completed_chars_pct: u8,
    },
    PromptKilled,
    PromptUndone,
    PickerOpened,
    PickerDismissed,
    PickerSearchChars {
        chars_typed: u8,
    },
    ArmToggled {
        armed: bool,
    },
    ExpressionError {
        kind: ExpressionErrorKind,
    },
    UpdateCheck {
        available: bool,
        current_version: &'static str,
    },
    UpdateApplied {
        from_version: &'static str,
        /// The version we updated to. `String` because it comes from the
        /// network (`update.version`), not a `env!()` literal.
        to_version: String,
    },
    SecureInputDetected,
    RdpDetected,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptMode {
    Stealth,
    Picker,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetAppKind {
    Browser,
    Native,
    Rdp,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CancelReason {
    UserKeystrokes,
    Esc,
    Error,
    Kill,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExpressionErrorKind {
    Syntax,
    Runtime,
    Timeout,
}

/// Initialize global telemetry. Must be called once at app startup.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        tracing::info!("telemetry init — aptabase key {}", APTABASE_KEY);
    });
}

/// Type-safe send: `event` is one of the whitelisted variants.
///
/// On macOS/Windows this dispatches to the `tauri-plugin-aptabase` plugin
/// (registered in `app::setup`). The plugin handles batching and offline
/// buffering. Per §12 we never include prompt content / triggers / expression
/// source — the `TelemetryEvent` enum itself enforces this at compile time.
pub fn send(app: &tauri::AppHandle, event: TelemetryEvent) {
    let payload = serde_json::to_value(&event).unwrap_or(Value::Null);
    let (name, props) = match payload {
        Value::Object(map) => {
            let name = map
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let props = map.get("props").cloned().unwrap_or(json!({}));
            (name, props)
        }
        _ => (String::new(), json!({})),
    };
    if name.is_empty() {
        return;
    }
    tracing::debug!("telemetry: {} {}", name, event.short_name());

    // Forward to Aptabase. The plugin exposes `EventTracker::track_event`
    // on the AppHandle; in tests / when the plugin isn't registered (unit
    // tests), `try_state` returns None and we silently drop.
    #[cfg(not(test))]
    forward_to_aptabase(app, &name, props);
    #[cfg(test)]
    {
        let _ = (app, props);
    }
}

#[cfg(not(test))]
fn forward_to_aptabase(app: &tauri::AppHandle, name: &str, props: Value) {
    // CI / E2E launches set PROMPT_PLAYER_E2E=1 — drop the event entirely so
    // automated installs/launches don't show up as real users in Aptabase.
    // Real users never set this var.
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
    fn short_name(&self) -> &'static str {
        match self {
            Self::AppStarted { .. } => "app_started",
            Self::PromptFired { .. } => "prompt_fired",
            Self::PromptCancelled { .. } => "prompt_cancelled",
            Self::PromptKilled => "prompt_killed",
            Self::PromptUndone => "prompt_undone",
            Self::PickerOpened => "picker_opened",
            Self::PickerDismissed => "picker_dismissed",
            Self::PickerSearchChars { .. } => "picker_search_chars",
            Self::ArmToggled { .. } => "arm_toggled",
            Self::ExpressionError { .. } => "expression_error",
            Self::UpdateCheck { .. } => "update_check",
            Self::UpdateApplied { .. } => "update_applied",
            Self::SecureInputDetected => "secure_input_detected",
            Self::RdpDetected => "rdp_detected",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check: no event variant carries a free-text field that could
    /// leak prompt content. We assert this via field naming + type discipline:
    /// every props field is bool/u8/enum, never `String` of user content.
    /// This test reads the JSON and ensures no value is a long string.
    #[test]
    fn no_event_payload_includes_long_strings() {
        let events = vec![
            TelemetryEvent::AppStarted {
                version: "0.1.0",
                os: "macos",
                locale: "en-US".into(),
                profile_in_use: "sales-engineer",
            },
            TelemetryEvent::PromptFired {
                mode: PromptMode::Stealth,
                char_count_bucket: CharBucket::Medium,
                has_expressions: false,
                target_app_kind: TargetAppKind::Native,
                scope_match: true,
            },
            TelemetryEvent::PromptCancelled {
                reason: CancelReason::UserKeystrokes,
                completed_chars_pct: 23,
            },
            TelemetryEvent::ExpressionError {
                kind: ExpressionErrorKind::Timeout,
            },
        ];
        for e in events {
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

    #[test]
    fn char_bucket_classify() {
        assert!(matches!(CharBucket::classify(10), CharBucket::Tiny));
        assert!(matches!(CharBucket::classify(50), CharBucket::Small));
        assert!(matches!(CharBucket::classify(200), CharBucket::Medium));
        assert!(matches!(CharBucket::classify(1000), CharBucket::Large));
        assert!(matches!(CharBucket::classify(5000), CharBucket::Huge));
    }
}
