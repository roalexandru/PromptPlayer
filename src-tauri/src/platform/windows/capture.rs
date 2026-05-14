//! Windows screen-capture exclusion helpers + foreground-target HWND classification.
//!
//! Two responsibilities, both small:
//!   1. Apply `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` to a window
//!      AND every descendant HWND in its tree. WebView2 hosts its GPU swap
//!      chain in a descendant HWND; applying the flag to the parent alone
//!      leaves the child swap chain visible-to-capture on some Win11 24H2
//!      configurations, which is the failure mode that produced "picker is
//!      invisible during Zoom share."
//!   2. Classify a foreground HWND so the picker's focus-snapshot can skip
//!      Zoom-share helper windows (which become transiently foreground during
//!      a share session) and our own windows (picker, tray helper) when
//!      picking the real target app to restore focus to.
//!
//! `platform::windows` is cfg-gated off non-Windows builds (see
//! `platform/mod.rs`), so this module's helpers compile only on Windows.

use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetForegroundWindow, GetWindow, GetWindowDisplayAffinity,
    GetWindowTextW, IsIconic, IsWindowVisible, SetWindowDisplayAffinity, GW_HWNDNEXT,
    WINDOW_DISPLAY_AFFINITY,
};

// --------------------------------------------------------------------------
// Pure helpers — `&str` matchers, unit-testable without any Win32 state.
// --------------------------------------------------------------------------

/// Class names of windows Zoom puts in the foreground transiently during a
/// screen-share session. We never want the picker's focus snapshot to capture
/// one of these as the "target" — the user's actual target app is the next
/// thing in z-order below them.
///
/// List is empirically derived from Zoom 5.x–6.x on Windows. Adding more here
/// is the right escape hatch if a future Zoom version introduces a new helper
/// class (no behavior change for non-Zoom users).
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

/// Our own top-level window classes. The foreground-snapshot filter must walk
/// past these when picking the real target — the picker is briefly foreground
/// between the user's commit keystroke and our `hide()` call.
///
/// Tauri's default top-level class is `"Tauri Window"`; the menu owner is
/// `"PromptPlayerMenuOwner"` (see `platform/windows/menu.rs::ensure_helper`).
pub fn is_own_window_class(class: &str) -> bool {
    matches!(class, "Tauri Window" | "PromptPlayerMenuOwner")
}

// --------------------------------------------------------------------------
// Win32 wrappers.
// --------------------------------------------------------------------------

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

/// True if DWM reports the window as cloaked (per-monitor hidden, virtual-
/// desktop hidden, or shell-cloaked). These windows are not real focus
/// targets — typing into them types into a window the user can't see.
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

/// Collect every descendant HWND under `parent`. `EnumChildWindows` walks the
/// full tree (children, grandchildren, …) per MSDN, so a single call is enough.
pub fn enumerate_descendants(parent: HWND) -> Vec<HWND> {
    if parent.0.is_null() {
        return Vec::new();
    }
    let mut collected: Vec<HWND> = Vec::new();
    // SAFETY: `lparam` is a pointer to a `Vec<HWND>` owned by this stack frame.
    // `EnumChildWindows` is synchronous — the callback only runs while we're
    // blocked on this call below, so the pointer is live for the duration.
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let v = &mut *(lparam.0 as *mut Vec<HWND>);
        v.push(hwnd);
        BOOL(1)
    }
    let lparam = LPARAM(&mut collected as *mut Vec<HWND> as isize);
    let _ = unsafe { EnumChildWindows(parent, Some(cb), lparam) };
    collected
}

/// Apply `affinity` (typically `WDA_EXCLUDEFROMCAPTURE` or `WDA_NONE`) to
/// `parent` and every descendant in its HWND tree. Per-HWND failures are
/// logged as warnings and don't abort the walk; the parent is set first so
/// even partial success preserves the prior single-HWND behavior.
///
/// Returns `(applied, attempted)` on success. Errs only when `parent` is null.
///
/// **Two known failure modes are surfaced via logging:**
///
/// 1. WebView2 hosts its GPU swap chain in a descendant HWND; the parent's
///    display-affinity flag does not propagate to that child on some
///    Win11 builds. Microsoft tracks this as WebView2Feedback #4544
///    (AB#50877897). Recursive application here is the workaround.
///
/// 2. On Windows 11, the kernel function `win32kfull.sys::ChangeWindowTreeProtection`
///    has a bug that makes `SetWindowDisplayAffinity` return
///    `ERROR_NOT_ENOUGH_MEMORY` (HRESULT `0x80070008`) for "non-traditional
///    Win32" apps including Chromium / WebView2 / Electron. Microsoft's
///    official workaround is the `LegacyDisplayAffinity` Application
///    Compatibility shim (see https://aka.ms/AppCompat). This function
///    cannot fix that — it can only detect and log it.
pub fn apply_affinity_recursive(
    parent: HWND,
    affinity: WINDOW_DISPLAY_AFFINITY,
) -> Result<(usize, usize), String> {
    if parent.0.is_null() {
        return Err("apply_affinity_recursive: null parent HWND".into());
    }
    let mut attempted = 0usize;
    let mut applied = 0usize;

    attempted += 1;
    match unsafe { SetWindowDisplayAffinity(parent, affinity) } {
        Ok(()) => {
            applied += 1;
            tracing::debug!(
                hwnd = parent.0 as usize,
                class = %class_name_of(parent),
                affinity = affinity.0,
                "display-affinity set (parent)"
            );
        }
        Err(e) => {
            // HRESULT 0x80070008 = Win32 ERROR_NOT_ENOUGH_MEMORY. On Windows
            // 11, this specific failure is the `ChangeWindowTreeProtection`
            // kernel bug — not an actual memory exhaustion. Surface it as
            // its own structured event so log-greppers can distinguish "the
            // OS rejected us" from "the HWND was destroyed mid-call".
            let hr = e.code().0 as u32;
            if hr == 0x8007_0008 {
                tracing::error!(
                    target: "prompt_player::capture",
                    hwnd = parent.0 as usize,
                    class = %class_name_of(parent),
                    hresult = format!("0x{hr:08X}"),
                    "win11_legacy_display_affinity_bug: SetWindowDisplayAffinity returned ERROR_NOT_ENOUGH_MEMORY (win32k bug). Capture-exclusion will NOT work until the LegacyDisplayAffinity Application Compatibility shim is applied. See https://learn.microsoft.com/en-us/answers/questions/700122/setwindowdisplayaffinity-on-windows-11"
                );
            } else {
                tracing::warn!(
                    target: "prompt_player::capture",
                    hwnd = parent.0 as usize,
                    class = %class_name_of(parent),
                    hresult = format!("0x{hr:08X}"),
                    "SetWindowDisplayAffinity on parent failed: {e}"
                );
            }
        }
    }

    for child in enumerate_descendants(parent) {
        attempted += 1;
        match unsafe { SetWindowDisplayAffinity(child, affinity) } {
            Ok(()) => {
                applied += 1;
                tracing::debug!(
                    hwnd = child.0 as usize,
                    class = %class_name_of(child),
                    affinity = affinity.0,
                    "display-affinity set (descendant)"
                );
            }
            Err(e) => {
                tracing::warn!(
                    hwnd = child.0 as usize,
                    class = %class_name_of(child),
                    "SetWindowDisplayAffinity on descendant failed: {e}"
                );
            }
        }
    }

    tracing::info!(
        target: "prompt_player::capture",
        parent = parent.0 as usize,
        applied,
        attempted,
        affinity = affinity.0,
        "display-affinity applied to picker tree"
    );

    Ok((applied, attempted))
}

/// Read the current display-affinity flag for a window. Returns `None` on
/// failure. Used by the T2 integration test to assert recursive-apply hit
/// every descendant.
pub fn current_display_affinity(hwnd: HWND) -> Option<WINDOW_DISPLAY_AFFINITY> {
    let mut out = WINDOW_DISPLAY_AFFINITY(0);
    let res = unsafe { GetWindowDisplayAffinity(hwnd, &mut out) };
    res.ok().map(|_| out)
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

// --------------------------------------------------------------------------
// CandidateWindow + select_target — pure z-order classifier used by
// picker/focus.rs::capture_foreground. Splitting the policy from the OS
// queries keeps F1–F5 testable without faking Win32.
// --------------------------------------------------------------------------

/// Snapshot of one top-level window's classification state. Constructed
/// from Win32 calls by `collect_z_order_candidates`, or by hand in tests.
#[derive(Debug, Clone)]
pub struct CandidateWindow {
    /// HWND value as a u64 so the struct is `Send` and doesn't carry an
    /// HWND through test data. Production code casts back via `HWND(_ as _)`
    /// when it needs the real handle.
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

/// Pure selection over a precomputed candidate list. Picks the first
/// acceptable entry, capped at 10. Logs the picked / rejected decisions at
/// debug level so the L2 logging test can pin them.
///
/// Returns `None` only if no candidate within the cap is acceptable; the
/// caller is expected to fall back to the raw foreground HWND rather than
/// silently drop the snapshot.
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

/// Build a candidate snapshot by walking z-order from `start`, up to `cap`
/// entries. Each entry's metadata is fetched once at collection time so the
/// result is a coherent moment-snapshot (no torn state from candidate i's
/// metadata being queried after candidate i+1's HWND moved).
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

// --------------------------------------------------------------------------
// T1 — pure-function unit tests.
// --------------------------------------------------------------------------

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
        assert!(is_own_window_class("Tauri Window"));
        assert!(is_own_window_class("PromptPlayerMenuOwner"));
    }

    #[test]
    fn non_own_classes_pass_through() {
        for c in [
            "Chrome_WidgetWin_1",
            "Notepad",
            "tauri window",       // case-sensitive
            "PromptPlayer",       // partial / prefix
            "ZPToolBarParentWnd", // share-helper, not own
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
        let own = ["Tauri Window", "PromptPlayerMenuOwner"];
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

    // ---------------------------------------------------------------------
    // T3 — F1..F5: select_target classifier tests (pure, no Win32 state).
    // ---------------------------------------------------------------------

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
        let cs = vec![cand("Tauri Window"), cand("Notepad")];
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
            cand("Tauri Window"),
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
        let mut cs: Vec<CandidateWindow> = (0..10).map(|_| cand("Tauri Window")).collect();
        cs.push(cand("Notepad"));
        assert!(
            select_target(&cs).is_none(),
            "select_target must respect the 10-iteration cap"
        );
    }

    #[test]
    fn select_target_picks_at_cap_boundary() {
        let mut cs: Vec<CandidateWindow> = (0..9).map(|_| cand("Tauri Window")).collect();
        cs.push(cand("Notepad"));
        let picked = select_target(&cs).expect("10th candidate is within the cap");
        assert_eq!(picked.class, "Notepad");
    }

    // ---------------------------------------------------------------------
    // T4 / L2 — logging snapshot tests for select_target's decisions.
    // ---------------------------------------------------------------------

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
