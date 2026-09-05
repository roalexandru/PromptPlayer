//! Focused-element inspection: is it safe to type here, and what is selected?
//!
//! Two spec promises depend on this and neither had an implementation:
//!
//! - §11 "fires in password field → password-field heuristic via Accessibility
//!   role". Nothing checked the focused element, so a prompt fired at a Finder
//!   window, a file dialog, or a password box typed into it regardless.
//! - §6.1 `$SELECTION`. The placeholder and expression contexts existed but
//!   were never populated, so every prompt referencing it expanded to nothing —
//!   including the shipped `02-refactor` example.
//!
//! The platform reads live in `platform/macos/ax.rs` (Accessibility API) and
//! `platform/windows/uia.rs` (UI Automation). Classification is pure and lives
//! here so it is testable on any host.
//!
//! ## Fail-open, deliberately
//! `Unknown` is the answer whenever the OS won't say — no Accessibility grant,
//! an unresponsive target, a Chromium/Electron surface that reports a generic
//! web area. Only `Secure` and `NotEditable` block a fire. A guard that
//! blocked legitimate targets would be worse than no guard: it would break
//! typing into exactly the editors this app exists to demo into.

use serde::Serialize;

/// What the OS says about the element that currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "kebab-case")]
pub enum FieldKind {
    /// A text field / text area / editable document. Safe to type into.
    Editable,
    /// A password or otherwise secure field. Never type here.
    Secure,
    /// Focus is on something that cannot accept text (a button, a list row,
    /// a Finder window). Typing would fire keyboard shortcuts instead.
    NotEditable,
    /// The OS didn't tell us enough to decide. Proceed.
    Unknown,
}

impl FieldKind {
    /// Whether a fire may proceed. Only the two confident negatives block.
    pub fn allows_typing(&self) -> bool {
        !matches!(self, Self::Secure | Self::NotEditable)
    }

    /// Short reason for logs and telemetry.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Editable => "editable",
            Self::Secure => "secure-field",
            Self::NotEditable => "not-editable",
            Self::Unknown => "unknown",
        }
    }
}

/// A snapshot of the focused element.
#[derive(Debug, Clone, Default)]
pub struct FocusedField {
    pub kind: FieldKindOpt,
    /// Platform role string, for logs (`AXTextArea`, `Edit`, …).
    pub role: Option<String>,
    /// Currently selected text, when the platform exposes it.
    pub selected_text: Option<String>,
}

/// `FieldKind` with a `Default` of `Unknown`, so `FocusedField::default()` is
/// the "we know nothing, allow it" state.
pub type FieldKindOpt = FieldKind;

impl Default for FieldKind {
    fn default() -> Self {
        Self::Unknown
    }
}

/// macOS AX roles that definitely cannot take typed text. Kept deliberately
/// short: anything not listed stays `Unknown` and is allowed. `AXWebArea`,
/// `AXGroup` and `AXScrollArea` are absent on purpose — Chromium, Electron
/// and terminal emulators report those for perfectly typeable surfaces.
const MAC_NON_TEXT_ROLES: &[&str] = &[
    "AXButton",
    "AXMenuItem",
    "AXMenuBarItem",
    "AXMenuButton",
    "AXCheckBox",
    "AXRadioButton",
    "AXSlider",
    "AXStepper",
    "AXImage",
    "AXStaticText",
    "AXLink",
    "AXTable",
    "AXOutline",
    "AXRow",
    "AXCell",
    "AXColumn",
    "AXDisclosureTriangle",
    "AXPopUpButton",
    "AXTabGroup",
    "AXToolbar",
    "AXSplitter",
    "AXProgressIndicator",
];

/// macOS AX roles that are text entry surfaces.
const MAC_TEXT_ROLES: &[&str] = &["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"];

/// Classify a macOS focused element from its role and subrole.
///
/// The subrole is checked first: a secure text field reports
/// `AXRole=AXTextField` with `AXSubrole=AXSecureTextField`, so looking only at
/// the role would classify a password box as a safe place to type.
pub fn classify_mac_role(role: Option<&str>, subrole: Option<&str>) -> FieldKind {
    if let Some(sub) = subrole {
        if sub == "AXSecureTextField" {
            return FieldKind::Secure;
        }
    }
    match role {
        Some(r) if MAC_TEXT_ROLES.contains(&r) => FieldKind::Editable,
        Some(r) if MAC_NON_TEXT_ROLES.contains(&r) => FieldKind::NotEditable,
        _ => FieldKind::Unknown,
    }
}

/// UI Automation control-type ids we treat as text entry.
/// 50004 Edit, 50030 Document, 50003 ComboBox, 50008 ListItem is NOT included.
const UIA_TEXT_TYPES: &[i32] = &[50004, 50030, 50003];
/// Control types that cannot take typed text: Button, CheckBox, RadioButton,
/// ListItem, MenuItem, Tab, TabItem, Tree, TreeItem, Image, Hyperlink,
/// ScrollBar, Slider, ProgressBar, DataGrid, DataItem, Header, HeaderItem.
const UIA_NON_TEXT_TYPES: &[i32] = &[
    50000, 50002, 50013, 50007, 50011, 50018, 50019, 50023, 50024, 50006, 50005, 50014, 50015,
    50012, 50028, 50029, 50034, 50035,
];

/// Classify a Windows focused element from its UIA control type and the
/// `IsPassword` property.
pub fn classify_uia_control(control_type: i32, is_password: bool) -> FieldKind {
    if is_password {
        return FieldKind::Secure;
    }
    if UIA_TEXT_TYPES.contains(&control_type) {
        return FieldKind::Editable;
    }
    if UIA_NON_TEXT_TYPES.contains(&control_type) {
        return FieldKind::NotEditable;
    }
    FieldKind::Unknown
}

/// Inspect the element with keyboard focus.
#[cfg(target_os = "macos")]
pub fn focused_field() -> FocusedField {
    crate::platform::macos::ax::focused_field()
}

#[cfg(target_os = "windows")]
pub fn focused_field() -> FocusedField {
    crate::platform::windows::uia::focused_field()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn focused_field() -> FocusedField {
    FocusedField::default()
}

/// Selected text in the focused element, for `$SELECTION`.
pub fn selected_text() -> Option<String> {
    let text = focused_field().selected_text?;
    (!text.is_empty()).then_some(text)
}

/// Title of the focused window of the frontmost app.
///
/// Windows gets this from `GetWindowTextW` already; macOS needs the AX API
/// (`NSWorkspace` exposes no window titles at all), which is why
/// `scopes::capture_macos` left `window_title` permanently `None` and every
/// `window-title-regex:` scope silently failed to match there.
#[cfg(target_os = "macos")]
pub fn focused_window_title() -> Option<String> {
    crate::platform::macos::ax::focused_window_title()
}

#[cfg(not(target_os = "macos"))]
pub fn focused_window_title() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_subrole_beats_a_text_role() {
        // The load-bearing case: a password box IS an AXTextField.
        assert_eq!(
            classify_mac_role(Some("AXTextField"), Some("AXSecureTextField")),
            FieldKind::Secure
        );
    }

    #[test]
    fn mac_text_roles_are_editable() {
        for r in ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"] {
            assert_eq!(classify_mac_role(Some(r), None), FieldKind::Editable, "{r}");
        }
    }

    #[test]
    fn mac_control_roles_are_not_editable() {
        for r in ["AXButton", "AXRow", "AXStaticText", "AXPopUpButton"] {
            assert_eq!(
                classify_mac_role(Some(r), None),
                FieldKind::NotEditable,
                "{r}"
            );
        }
    }

    #[test]
    fn mac_web_and_container_roles_stay_unknown() {
        // Chromium, Electron and terminals report these for typeable surfaces;
        // classifying them would break the app's main targets.
        for r in [
            "AXWebArea",
            "AXGroup",
            "AXScrollArea",
            "AXUnknown",
            "AXWindow",
        ] {
            assert_eq!(classify_mac_role(Some(r), None), FieldKind::Unknown, "{r}");
        }
        assert_eq!(classify_mac_role(None, None), FieldKind::Unknown);
    }

    #[test]
    fn uia_password_flag_beats_control_type() {
        assert_eq!(classify_uia_control(50004, true), FieldKind::Secure);
    }

    #[test]
    fn uia_edit_and_document_are_editable() {
        assert_eq!(classify_uia_control(50004, false), FieldKind::Editable);
        assert_eq!(classify_uia_control(50030, false), FieldKind::Editable);
    }

    #[test]
    fn uia_button_is_not_editable() {
        assert_eq!(classify_uia_control(50000, false), FieldKind::NotEditable);
    }

    #[test]
    fn uia_unlisted_types_stay_unknown() {
        // 50032 (Window), 50033 (Pane) and anything we didn't enumerate.
        for t in [50032, 50033, 0, -1, 99999] {
            assert_eq!(classify_uia_control(t, false), FieldKind::Unknown, "{t}");
        }
    }

    #[test]
    fn uia_text_and_non_text_lists_are_disjoint() {
        for t in UIA_TEXT_TYPES {
            assert!(
                !UIA_NON_TEXT_TYPES.contains(t),
                "control type {t} is in both lists"
            );
        }
    }

    #[test]
    fn mac_text_and_non_text_lists_are_disjoint() {
        for r in MAC_TEXT_ROLES {
            assert!(!MAC_NON_TEXT_ROLES.contains(r), "role {r} is in both lists");
        }
    }

    #[test]
    fn only_confident_negatives_block_typing() {
        assert!(FieldKind::Editable.allows_typing());
        assert!(FieldKind::Unknown.allows_typing(), "fail open");
        assert!(!FieldKind::Secure.allows_typing());
        assert!(!FieldKind::NotEditable.allows_typing());
    }

    #[test]
    fn default_is_permissive() {
        assert_eq!(FieldKind::default(), FieldKind::Unknown);
        assert!(FocusedField::default().kind.allows_typing());
    }
}
