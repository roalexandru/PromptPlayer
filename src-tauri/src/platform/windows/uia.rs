//! Windows UI Automation reads: focused-element control type, password flag,
//! and text selection.
//!
//! Mirrors `platform/macos/ax.rs` so `crate::accessibility` can dispatch
//! without branching. UIA is the only API that sees inside modern toolkits —
//! `GetGUIThreadInfo` plus window classes tells you nothing useful about a
//! Chromium, WPF, or Electron surface, which is most of what this app types
//! into.
//!
//! Every step is fallible and every failure degrades to "unknown", which the
//! caller treats as permission to type. UIA is a courtesy here, not a gate.

use crate::accessibility::{classify_uia_control, FieldKind, FocusedField};
use windows::core::Interface;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId,
};

/// Cap on how much selected text we pull across the process boundary. A
/// `$SELECTION` of more than this is not a prompt input, it's an accident.
const MAX_SELECTION_CHARS: i32 = 64 * 1024;

/// Initializes COM for the calling thread and undoes it on drop.
///
/// The fire pipeline spawns a fresh thread per fire, so COM is never already
/// initialized in practice — but `RPC_E_CHANGED_MODE` (a thread that joined a
/// different apartment) must not be treated as ours to uninitialize, or we'd
/// tear down COM under whoever set it up.
struct ComGuard {
    owned: bool,
}

impl ComGuard {
    fn new() -> Self {
        // SAFETY: standard COM initialization for this thread; the matching
        // `CoUninitialize` happens in `Drop` only when this call succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            tracing::debug!("thread already in a different COM apartment; not uninitializing");
            return Self { owned: false };
        }
        Self { owned: hr.is_ok() }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { CoUninitialize() }
        }
    }
}

/// Read the selected text out of an element via its TextPattern.
///
/// Not every control implements TextPattern (a plain Win32 Edit often exposes
/// only ValuePattern), so `None` here is routine.
unsafe fn selection_of(
    element: &windows::Win32::UI::Accessibility::IUIAutomationElement,
) -> Option<String> {
    let pattern = element.GetCurrentPattern(UIA_TextPatternId).ok()?;
    let text: IUIAutomationTextPattern = pattern.cast().ok()?;
    let ranges = text.GetSelection().ok()?;
    if ranges.Length().ok()? < 1 {
        return None;
    }
    let range = ranges.GetElement(0).ok()?;
    let bstr = range.GetText(MAX_SELECTION_CHARS).ok()?;
    let s = bstr.to_string();
    (!s.is_empty()).then_some(s)
}

/// Inspect the element with keyboard focus.
pub fn focused_field() -> FocusedField {
    let _com = ComGuard::new();
    // SAFETY: all calls below are COM calls on interfaces obtained in this
    // scope; each is checked and any failure returns the permissive default.
    unsafe {
        let automation: IUIAutomation =
            match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
                Ok(a) => a,
                Err(e) => {
                    tracing::debug!("UIAutomation unavailable: {e}");
                    return FocusedField::default();
                }
            };
        let element = match automation.GetFocusedElement() {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("GetFocusedElement failed: {e}");
                return FocusedField::default();
            }
        };
        let control_type = element.CurrentControlType().map(|t| t.0).unwrap_or(0);
        let is_password = element
            .CurrentIsPassword()
            .map(|b| b.as_bool())
            .unwrap_or(false);
        let kind = classify_uia_control(control_type, is_password);
        let role = element
            .CurrentClassName()
            .ok()
            .map(|b| b.to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| Some(format!("uia:{control_type}")));
        // Never copy a password out of a secure field.
        let selected_text = if kind == FieldKind::Secure {
            None
        } else {
            selection_of(&element)
        };
        FocusedField {
            kind,
            role,
            selected_text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // As with the macOS AX tests, the contract worth pinning on a headless
    // runner is "never panics, never blocks, degrades to permissive" — this
    // code runs on the fire path in front of every keystroke.

    #[test]
    fn focused_field_survives_a_headless_session() {
        let f = focused_field();
        if f.kind == FieldKind::Unknown {
            assert!(f.kind.allows_typing());
        }
    }

    #[test]
    fn repeated_queries_do_not_crash_com() {
        // Each call initializes and uninitializes COM on this thread; a
        // mismatched pair shows up here.
        for _ in 0..20 {
            let _ = focused_field();
        }
    }

    #[test]
    fn com_guard_is_reentrant_within_a_thread() {
        let outer = ComGuard::new();
        let inner = ComGuard::new();
        drop(inner);
        // The outer guard must still be able to uninitialize cleanly.
        drop(outer);
        let _ = focused_field();
    }
}
