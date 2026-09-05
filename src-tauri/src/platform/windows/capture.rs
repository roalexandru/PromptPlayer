//! Two things: apply `WDA_EXCLUDEFROMCAPTURE` to the picker's top-level HWND
//! (with a `WDA_MONITOR` fallback for the Win11 win32k bug), and classify a
//! foreground HWND so the focus snapshot skips Zoom-share and our own helpers.
//!
//! No descendant walk — the affinity API is top-level only.

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindow, GetWindowDisplayAffinity, GetWindowTextW,
    IsIconic, IsWindowVisible, SetWindowDisplayAffinity, GW_HWNDNEXT, WDA_EXCLUDEFROMCAPTURE,
    WDA_MONITOR, WINDOW_DISPLAY_AFFINITY,
};

// Pure helpers — `&str` matchers, unit-testable without any Win32 state.

/// Zoom's transient share windows, skipped by the focus snapshot — the real
/// target sits below them in z-order. Empirical from Zoom 5.x–6.x.
pub fn is_known_share_helper(class: &str) -> bool {
    matches!(
        class,
        "ZPToolBarParentWnd"
            | "ZPContentViewWndClass"
            | "ZPFloatVideoWndClass"
            | "Zoom_ShareControlBar"
            | "ZPMeetingMainFrameClassForWindow"
            | "ZPSharingFloatToolbarClass"
    )
}

/// Our own windows that must never be a focus target — just the tray-menu
/// owner. Tauri's shared class is absent: library/About are legitimate.
pub fn is_own_window_class(class: &str) -> bool {
    matches!(class, "PromptPlayerMenuOwner")
}

// Win32 wrappers.

/// Fetch the class name of `hwnd`. Returns `""` for the null HWND or when
/// the OS call fails (destroyed window, invalid handle).
pub fn class_name_of(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

/// DWM-cloaked windows aren't real focus targets — typing into one types
/// somewhere the user can't see.
fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    let res = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            core::mem::size_of::<u32>() as u32,
        )
    };
    res.is_ok() && cloaked != 0
}

/// Apply `affinity` to a top-level `hwnd`, returning what's actually in effect.
/// Child windows are rejected; a Win11 win32k bug forces a `WDA_MONITOR` fallback.
pub fn apply_display_affinity(
    hwnd: HWND,
    affinity: WINDOW_DISPLAY_AFFINITY,
) -> Result<WINDOW_DISPLAY_AFFINITY, String> {
    if hwnd.0.is_null() {
        return Err("apply_display_affinity: null HWND".into());
    }
    let class = class_name_of(hwnd);
    let err = match unsafe { SetWindowDisplayAffinity(hwnd, affinity) } {
        Ok(()) => {
            tracing::info!(
                target: "prompt_player::capture",
                hwnd = hwnd.0 as usize,
                class = %class,
                affinity = affinity.0,
                "display-affinity applied"
            );
            return Ok(affinity);
        }
        Err(e) => e,
    };

    // 0x80070008 here is the `ChangeWindowTreeProtection` kernel bug, not real
    // memory exhaustion — its own event so it greps apart from a dead HWND.
    let hr = err.code().0 as u32;
    if hr != 0x8007_0008 {
        tracing::warn!(
            target: "prompt_player::capture",
            hwnd = hwnd.0 as usize,
            class = %class,
            hresult = format!("0x{hr:08X}"),
            "SetWindowDisplayAffinity failed: {err}"
        );
        return Err(format!("SetWindowDisplayAffinity: {err}"));
    }
    tracing::error!(
        target: "prompt_player::capture",
        hwnd = hwnd.0 as usize,
        class = %class,
        hresult = format!("0x{hr:08X}"),
        "win11_legacy_display_affinity_bug: SetWindowDisplayAffinity returned ERROR_NOT_ENOUGH_MEMORY (win32k bug). Capture-exclusion will NOT work until the LegacyDisplayAffinity Application Compatibility shim is applied. See https://learn.microsoft.com/en-us/answers/questions/700122/setwindowdisplayaffinity-on-windows-11"
    );
    if affinity != WDA_EXCLUDEFROMCAPTURE {
        return Err(format!("SetWindowDisplayAffinity: {err}"));
    }
    match unsafe { SetWindowDisplayAffinity(hwnd, WDA_MONITOR) } {
        Ok(()) => {
            tracing::warn!(
                target: "prompt_player::capture",
                hwnd = hwnd.0 as usize,
                class = %class,
                "fell back to WDA_MONITOR after WDA_EXCLUDEFROMCAPTURE rejected by win32k bug; audience sees a black rectangle instead of see-through"
            );
            Ok(WDA_MONITOR)
        }
        Err(e2) => Err(format!(
            "SetWindowDisplayAffinity: {err}; WDA_MONITOR fallback also failed: {e2}"
        )),
    }
}

/// Current display-affinity flag, or `None` on failure — which includes every
/// non-top-level window.
pub fn current_display_affinity(hwnd: HWND) -> Option<WINDOW_DISPLAY_AFFINITY> {
    // `GetWindowDisplayAffinity` is bound with a raw `*mut u32` out-param
    // (the newtype is only used on the `Set` side), so read into a `u32`.
    let mut raw: u32 = 0;
    let res = unsafe { GetWindowDisplayAffinity(hwnd, &mut raw) };
    res.ok().map(|_| WINDOW_DISPLAY_AFFINITY(raw))
}

/// Thin wrapper around `GetForegroundWindow` — kept here so the focus module's
/// `capture_foreground_with` test seam can mock it without touching Win32.
pub fn foreground_hwnd() -> HWND {
    unsafe { GetForegroundWindow() }
}

/// Fetch a window's title via `GetWindowTextW`. Empty title (or failure) →
/// `None`. Used by the foreground-snapshot path to label log entries.
pub fn window_title_of(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len > 0 {
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    } else {
        None
    }
}

// Pure z-order classifier for `picker::focus`. Splitting policy from the OS
// queries keeps it testable without faking Win32.

/// Snapshot of one top-level window's classification state. Constructed
/// from Win32 calls by `collect_z_order_candidates`, or by hand in tests.
#[derive(Debug, Clone)]
pub struct CandidateWindow {
    /// HWND as a u64 so the struct is `Send` and test data carries no handles.
    pub hwnd_raw: u64,
    pub class: String,
    pub title: Option<String>,
    pub visible: bool,
    pub cloaked: bool,
    pub iconic: bool,
}

impl CandidateWindow {
    /// True iff this window is a plausible focus-restore target.
    pub fn is_acceptable_target(&self) -> bool {
        self.visible
            && !self.cloaked
            && !self.iconic
            && !is_known_share_helper(&self.class)
            && !is_own_window_class(&self.class)
    }
}

/// First acceptable candidate, capped at 10, logging each decision at debug.
/// `None` means the caller should fall back to the raw foreground HWND.
pub fn select_target(candidates: &[CandidateWindow]) -> Option<&CandidateWindow> {
    for (i, c) in candidates.iter().take(10).enumerate() {
        if c.is_acceptable_target() {
            tracing::debug!(
                target: "prompt_player::capture",
                index = i,
                class = %c.class,
                "select_target: accepted candidate"
            );
            return Some(c);
        }
        let reason = if !c.visible {
            "not_visible"
        } else if c.cloaked {
            "cloaked"
        } else if c.iconic {
            "iconic"
        } else if is_known_share_helper(&c.class) {
            "share_helper"
        } else if is_own_window_class(&c.class) {
            "own_class"
        } else {
            "unknown"
        };
        tracing::debug!(
            target: "prompt_player::capture",
            index = i,
            class = %c.class,
            reason,
            "select_target: rejected candidate"
        );
    }
    None
}

/// Walk z-order from `start`, up to `cap`. Metadata is read once per entry so
/// the snapshot is coherent rather than torn across the walk.
pub fn collect_z_order_candidates(start: HWND, cap: usize) -> Vec<CandidateWindow> {
    let mut out = Vec::with_capacity(cap);
    let mut cur = start;
    for _ in 0..cap {
        if cur.0.is_null() {
            break;
        }
        out.push(snapshot_candidate(cur));
        cur = match unsafe { GetWindow(cur, GW_HWNDNEXT) } {
            Ok(h) if !h.0.is_null() => h,
            _ => break,
        };
    }
    out
}

fn snapshot_candidate(hwnd: HWND) -> CandidateWindow {
    CandidateWindow {
        hwnd_raw: hwnd.0 as u64,
        class: class_name_of(hwnd),
        title: window_title_of(hwnd),
        visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
        iconic: unsafe { IsIconic(hwnd) }.as_bool(),
        cloaked: is_cloaked(hwnd),
    }
}

// T1 — pure-function unit tests.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_helper_classes_recognised() {
        for c in [
            "ZPToolBarParentWnd",
            "ZPContentViewWndClass",
            "ZPFloatVideoWndClass",
            "Zoom_ShareControlBar",
            "ZPMeetingMainFrameClassForWindow",
            "ZPSharingFloatToolbarClass",
        ] {
            assert!(is_known_share_helper(c), "expected share-helper: {c}");
        }
    }

    #[test]
    fn non_share_classes_pass_through() {
        for c in [
            "Chrome_WidgetWin_1",
            "Notepad",
            "CASCADIA_HOSTING_WINDOW_CLASS",
            "MozillaWindowClass",
            "",
            "tauri window",       // lowercase, must NOT match Tauri's
            "zptoolbarparentwnd", // case-sensitive Win32 class names
            "Chrome_RenderWidgetHostHWND",
        ] {
            assert!(
                !is_known_share_helper(c),
                "unexpected share-helper match: {c}"
            );
        }
    }

    #[test]
    fn own_classes_recognised() {
        assert!(is_own_window_class("PromptPlayerMenuOwner"));
    }

    #[test]
    fn tauri_window_class_is_a_valid_restore_target() {
        // Skipping Tauri's shared class would redirect a prompt meant for the
        // library into whatever sits beneath it in z-order.
        assert!(!is_own_window_class("Tauri Window"));
    }

    #[test]
    fn non_own_classes_pass_through() {
        for c in [
            "Chrome_WidgetWin_1",
            "Notepad",
            "Tauri Window",          // our webview windows — legitimate targets
            "promptplayermenuowner", // case-sensitive
            "PromptPlayer",          // partial / prefix
            "ZPToolBarParentWnd",    // share-helper, not own
            "",
        ] {
            assert!(!is_own_window_class(c), "unexpected own-class match: {c}");
        }
    }

    #[test]
    fn share_and_own_are_disjoint() {
        let share = [
            "ZPToolBarParentWnd",
            "ZPContentViewWndClass",
            "ZPFloatVideoWndClass",
            "Zoom_ShareControlBar",
            "ZPMeetingMainFrameClassForWindow",
            "ZPSharingFloatToolbarClass",
        ];
        let own = ["PromptPlayerMenuOwner"];
        for s in share {
            assert!(!is_own_window_class(s), "share class also matched own: {s}");
        }
        for o in own {
            assert!(
                !is_known_share_helper(o),
                "own class also matched share: {o}"
            );
        }
    }

    // T3 — select_target classifier tests (pure, no Win32 state).

    fn cand(class: &str) -> CandidateWindow {
        CandidateWindow {
            hwnd_raw: 0x1000,
            class: class.into(),
            title: Some(format!("{class} title")),
            visible: true,
            cloaked: false,
            iconic: false,
        }
    }

    #[test]
    fn f1_share_helper_at_head_is_skipped() {
        let cs = vec![cand("ZPToolBarParentWnd"), cand("Chrome_WidgetWin_1")];
        let picked = select_target(&cs).expect("must pick the non-share-helper");
        assert_eq!(picked.class, "Chrome_WidgetWin_1");
    }

    #[test]
    fn f2_own_class_at_head_is_skipped() {
        let cs = vec![cand("PromptPlayerMenuOwner"), cand("Notepad")];
        let picked = select_target(&cs).expect("must pick past our own window");
        assert_eq!(picked.class, "Notepad");
    }

    #[test]
    fn f3_cloaked_iconic_invisible_are_skipped() {
        let mut cloaked = cand("Notepad");
        cloaked.cloaked = true;
        let mut iconic = cand("Slack");
        iconic.iconic = true;
        let mut hidden = cand("Edge");
        hidden.visible = false;
        let visible = cand("Code");
        let cs = vec![cloaked, iconic, hidden, visible];
        let picked = select_target(&cs).expect("must reach the first visible candidate");
        assert_eq!(picked.class, "Code");
    }

    #[test]
    fn f4_no_acceptable_returns_none() {
        let cs = vec![
            cand("ZPToolBarParentWnd"),
            cand("ZPContentViewWndClass"),
            cand("PromptPlayerMenuOwner"),
            {
                let mut c = cand("Notepad");
                c.iconic = true;
                c
            },
        ];
        assert!(
            select_target(&cs).is_none(),
            "no acceptable candidate must return None"
        );
    }

    #[test]
    fn f5_normal_foreground_is_picked_unchanged() {
        // Locks in the 99% case: a normal foreground class is selected as
        // the target with no walking past — same behavior as pre-Layer-C.
        for c in ["Chrome_WidgetWin_1", "Notepad", "MozillaWindowClass"] {
            let cs = vec![cand(c)];
            let picked = select_target(&cs).unwrap_or_else(|| panic!("must pick {c}"));
            assert_eq!(picked.class, c);
        }
    }

    #[test]
    fn select_target_caps_at_10() {
        let mut cs: Vec<CandidateWindow> = (0..10).map(|_| cand("PromptPlayerMenuOwner")).collect();
        cs.push(cand("Notepad"));
        assert!(
            select_target(&cs).is_none(),
            "select_target must respect the 10-iteration cap"
        );
    }

    #[test]
    fn select_target_picks_at_cap_boundary() {
        let mut cs: Vec<CandidateWindow> = (0..9).map(|_| cand("PromptPlayerMenuOwner")).collect();
        cs.push(cand("Notepad"));
        let picked = select_target(&cs).expect("10th candidate is within the cap");
        assert_eq!(picked.class, "Notepad");
    }

    // T4 / L2 — logging snapshot tests for select_target's decisions.

    #[tracing_test::traced_test]
    #[test]
    fn select_target_logs_accepted_decision() {
        let cs = vec![cand("Notepad")];
        let _ = select_target(&cs);
        assert!(
            logs_contain("accepted candidate"),
            "expected an 'accepted candidate' log line"
        );
    }

    #[tracing_test::traced_test]
    #[test]
    fn select_target_logs_rejection_reason() {
        let cs = vec![cand("ZPToolBarParentWnd"), cand("Notepad")];
        let _ = select_target(&cs);
        assert!(logs_contain("rejected candidate"));
        assert!(
            logs_contain("share_helper"),
            "rejection reason for ZP class must surface in the log"
        );
    }

    #[tracing_test::traced_test]
    #[test]
    fn select_target_logs_cloaked_rejection() {
        let mut c = cand("Notepad");
        c.cloaked = true;
        let cs = vec![c];
        let _ = select_target(&cs);
        assert!(logs_contain("cloaked"));
    }
}
