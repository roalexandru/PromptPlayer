//! The tray "menu" has two independent implementations — a Svelte popover on
//! macOS, a native `HMENU` on Windows — and `CLAUDE.md` warns that adding an
//! item means editing both. They drifted anyway: the Windows menu shipped with
//! no update rows (so `UpdateAvailableShown` was a macOS-only metric), no way
//! to abort a run in flight, and no Keep Awake duration, which left
//! `set_keep_awake_duration` unreachable from any Windows UI.
//!
//! Source-level, because the macOS side is a webview and the Windows side is a
//! Win32 call — there is no shared runtime object to compare. It catches the
//! drift that actually happened: a capability present in one and absent in the
//! other.

const MENU_RS: &str = include_str!("../src/platform/windows/menu.rs");
const POPOVER: &str = include_str!("../../src/windows/tray-popup.svelte");

/// One capability, and how it appears on each side.
struct Parity {
    what: &'static str,
    /// A marker that must appear in the Windows native menu.
    windows: &'static str,
    /// A marker that must appear in the macOS popover.
    macos: &'static str,
}

const REQUIRED: &[Parity] = &[
    Parity {
        what: "install an available update",
        windows: "ID_UPDATE_INSTALL",
        macos: "updaterInstall",
    },
    Parity {
        what: "skip an available update",
        windows: "ID_UPDATE_SKIP",
        macos: "updaterDismiss",
    },
    Parity {
        what: "abort a run in flight",
        windows: "ID_KILL",
        macos: "ipc.kill",
    },
    Parity {
        what: "choose the Keep Awake auto-off",
        windows: "ID_KEEP_AWAKE_DURATION_BASE",
        macos: "setKeepAwakeDuration",
    },
    Parity {
        what: "warn that the picker may be visible on a share",
        windows: "ID_CAPTURE_WARNING",
        macos: "captureDegraded",
    },
    Parity {
        what: "warn about a dead keyboard hook",
        windows: "ID_HOOK_WARNING",
        macos: "hookStatus",
    },
    Parity {
        what: "open diagnostics",
        windows: "ID_DIAGNOSTICS",
        macos: "openDiagnostics",
    },
    Parity {
        what: "fire a pinned prompt",
        windows: "ID_PINNED_BASE",
        macos: "trayFirePrompt",
    },
];

#[test]
fn both_tray_implementations_offer_the_same_capabilities() {
    let mut missing = Vec::new();
    for p in REQUIRED {
        if !MENU_RS.contains(p.windows) {
            missing.push(format!(
                "Windows tray menu cannot {}: `{}` is absent from menu.rs",
                p.what, p.windows
            ));
        }
        if !POPOVER.contains(p.macos) {
            missing.push(format!(
                "macOS tray popover cannot {}: `{}` is absent from tray-popup.svelte",
                p.what, p.macos
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "the two tray implementations have drifted:\n  {}",
        missing.join("\n  ")
    );
}

/// A menu id that is built but never dispatched is a dead row: the user picks
/// it and nothing happens.
#[test]
fn every_windows_menu_id_is_dispatched() {
    let ids: Vec<&str> = MENU_RS
        .lines()
        .filter_map(|l| l.trim().strip_prefix("const ID_"))
        .filter_map(|rest| rest.split(':').next())
        .collect();
    assert!(ids.len() > 8, "parsed too few menu ids — parser broke");

    let dispatch_start = MENU_RS
        .find("fn dispatch(")
        .expect("menu.rs must have a dispatch fn");
    let dispatch = &MENU_RS[dispatch_start..];

    for id in ids {
        let full = format!("ID_{id}");
        // `ID_PINNED_BASE` and the keep-awake base are range arms, not literal
        // match arms, but they must still appear in the dispatch body.
        assert!(
            dispatch.contains(&full),
            "menu item `{full}` is added to the menu but never handled in \
             dispatch — selecting it does nothing"
        );
    }
}
