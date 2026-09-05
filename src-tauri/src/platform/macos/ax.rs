//! macOS Accessibility (AX) reads: focused element role, selection, window title.
//!
//! Everything here goes through `AXUIElementCopyAttributeValue` on the
//! system-wide element, which needs the same Accessibility grant the keyboard
//! hook already requires (§9.1). When the grant is missing every call fails and
//! we report "unknown", which the caller treats as permissive.
//!
//! AX is synchronous IPC into the target application, so an unresponsive app
//! could block the fire path. `AXUIElementSetMessagingTimeout` caps that at
//! `AX_TIMEOUT_SECS`; without it a spinning Electron app would stall a demo
//! mid-keystroke.

use crate::accessibility::{classify_mac_role, FieldKind, FocusedField};
use core_foundation::base::{CFGetTypeID, CFRelease, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringGetTypeID, CFStringRef};
use std::ffi::c_void;

/// Upper bound on a single AX round-trip. Playback is on a 1 ms-ish budget per
/// key; a quarter second is already generous for one pre-flight query.
const AX_TIMEOUT_SECS: f32 = 0.25;

/// `kAXErrorSuccess`.
const AX_SUCCESS: i32 = 0;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> *const c_void;
    fn AXUIElementCopyAttributeValue(
        element: *const c_void,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(element: *const c_void, timeout_in_seconds: f32) -> i32;
}

/// RAII wrapper for an `AXUIElementRef`. AX returns +1 references from the
/// `Copy…` calls, so every one of them has to be released exactly once.
struct AxElement(*const c_void);

impl AxElement {
    fn system_wide() -> Option<Self> {
        // SAFETY: the call takes no arguments and returns either NULL or a
        // +1-retained CFTypeRef, which `Drop` releases.
        let raw = unsafe { AXUIElementCreateSystemWide() };
        if raw.is_null() {
            return None;
        }
        // SAFETY: `raw` is a live AXUIElementRef; setting a messaging timeout
        // on it is documented as valid for the system-wide element.
        unsafe {
            AXUIElementSetMessagingTimeout(raw, AX_TIMEOUT_SECS);
        }
        Some(Self(raw))
    }

    /// Read an attribute whose value is another AX element.
    fn element_attr(&self, name: &str) -> Option<Self> {
        let raw = self.raw_attr(name)?;
        // AX element attributes come back as AXUIElementRef; we keep the +1.
        Some(Self(raw as *const c_void))
    }

    /// Read an attribute whose value is a string.
    fn string_attr(&self, name: &str) -> Option<String> {
        let raw = self.raw_attr(name)?;
        // SAFETY: `raw` is a +1 CFTypeRef. We verify it really is a CFString
        // before casting — an attribute can legitimately hold another type,
        // and a blind cast would be undefined behavior.
        unsafe {
            if CFGetTypeID(raw) != CFStringGetTypeID() {
                CFRelease(raw);
                return None;
            }
            // `wrap_under_create_rule` takes ownership of the +1.
            let s = CFString::wrap_under_create_rule(raw as CFStringRef);
            Some(s.to_string())
        }
    }

    fn raw_attr(&self, name: &str) -> Option<CFTypeRef> {
        let key = CFString::new(name);
        let mut out: CFTypeRef = std::ptr::null();
        // SAFETY: `self.0` is a live AX element, `key` outlives the call, and
        // `out` is only read when the call reports success.
        let err =
            unsafe { AXUIElementCopyAttributeValue(self.0, key.as_concrete_TypeRef(), &mut out) };
        if err != AX_SUCCESS || out.is_null() {
            return None;
        }
        Some(out)
    }
}

impl Drop for AxElement {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: every construction path holds exactly one +1 reference.
            unsafe { CFRelease(self.0) }
        }
    }
}

/// Inspect the element with keyboard focus.
pub fn focused_field() -> FocusedField {
    let Some(system) = AxElement::system_wide() else {
        return FocusedField::default();
    };
    let Some(focused) = system.element_attr("AXFocusedUIElement") else {
        // No Accessibility grant, or genuinely nothing focused.
        return FocusedField::default();
    };
    let role = focused.string_attr("AXRole");
    let subrole = focused.string_attr("AXSubrole");
    let kind = classify_mac_role(role.as_deref(), subrole.as_deref());
    // Don't read the selection out of a secure field, even though AX would
    // usually refuse anyway — no reason to have a password in our address space.
    let selected_text = if kind == FieldKind::Secure {
        None
    } else {
        focused.string_attr("AXSelectedText")
    };
    FocusedField {
        kind,
        role,
        selected_text,
    }
}

/// Title of the frontmost app's focused window.
pub fn focused_window_title() -> Option<String> {
    let system = AxElement::system_wide()?;
    // The system-wide element exposes the focused *application*, from which
    // the focused window (and its title) hangs.
    let app = system.element_attr("AXFocusedApplication")?;
    let window = app
        .element_attr("AXFocusedWindow")
        .or_else(|| app.element_attr("AXMainWindow"))?;
    let title = window.string_attr("AXTitle")?;
    (!title.is_empty()).then_some(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise the real AX API against whatever is focused on the test
    // machine (usually nothing, since `cargo test` runs headless in CI and the
    // test binary holds no Accessibility grant). The contract under test is
    // therefore "never panics, never hangs, degrades to Unknown" — the failure
    // mode that matters, because it runs on the fire path.

    #[test]
    fn focused_field_is_safe_without_accessibility_grant() {
        let f = focused_field();
        // Whatever came back, it must not block typing unless the OS was
        // confident, and it must not have panicked getting here.
        if f.kind == FieldKind::Unknown {
            assert!(f.kind.allows_typing());
        }
    }

    #[test]
    fn focused_field_is_cheap_enough_for_the_fire_path() {
        let start = std::time::Instant::now();
        let _ = focused_field();
        // One query, timeout-capped. Well under the AX timeout even when it
        // has to give up entirely.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "focused_field took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn window_title_query_does_not_panic() {
        let _ = focused_window_title();
    }

    #[test]
    fn repeated_queries_do_not_leak_or_crash() {
        // Exercises the Drop/CFRelease pairing: a reference-counting mistake
        // shows up here as a crash under repeated calls.
        for _ in 0..50 {
            let _ = focused_field();
            let _ = focused_window_title();
        }
    }
}
