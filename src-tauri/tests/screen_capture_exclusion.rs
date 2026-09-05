//! `SetWindowDisplayAffinity` against a bare Win32 parent/child pair — no
//! Tauri, so the linkage issue that gates `ipc_registry.rs` off Windows
//! doesn't apply. The child case pins why `capture.rs` has no descendant walk.

#![cfg(target_os = "windows")]

use prompt_player::platform::windows::capture::{apply_display_affinity, current_display_affinity};
use std::sync::atomic::{AtomicU32, Ordering};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, SetWindowDisplayAffinity,
    UnregisterClassW, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WINDOW_DISPLAY_AFFINITY, WINDOW_EX_STYLE,
    WNDCLASSEXW, WNDCLASS_STYLES, WS_CHILD, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};

/// Unique class name per test — registration is process-global, so a reused
/// name fails the second `RegisterClassExW`.
static CLASS_COUNTER: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// RAII for the test windows, so a panicking test can't poison the next one
/// with a leftover class registration.
struct Tree {
    parent: HWND,
    child: HWND,
    class_name: Vec<u16>,
    hinst: HINSTANCE,
}

impl Tree {
    fn new() -> Self {
        let n = CLASS_COUNTER.fetch_add(1, Ordering::SeqCst);
        let class_name = wide(&format!("PromptPlayerCaptureTest_{n}"));

        unsafe {
            let hmodule = GetModuleHandleW(None).expect("GetModuleHandleW");
            let hinst = HINSTANCE(hmodule.0);

            let wnd_class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: WNDCLASS_STYLES(0),
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: Default::default(),
                hCursor: Default::default(),
                hbrBackground: Default::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                hIconSm: Default::default(),
            };
            let atom = RegisterClassExW(&wnd_class);
            assert!(atom != 0, "RegisterClassExW must succeed");

            // Parent: off-screen top-level popup. Not WS_VISIBLE; we don't
            // want a visible test artifact flashing on the user's screen.
            let parent = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                PCWSTR(class_name.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                -32000,
                -32000,
                1,
                1,
                None,
                None,
                hinst,
                None,
            )
            .expect("CreateWindowExW parent");

            let child = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class_name.as_ptr()),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                1,
                1,
                parent,
                None,
                hinst,
                None,
            )
            .expect("CreateWindowExW child");

            Tree {
                parent,
                child,
                class_name,
                hinst,
            }
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        unsafe {
            // DestroyWindow on the top-level cascades to children.
            let _ = DestroyWindow(self.parent);
            let _ = UnregisterClassW(PCWSTR(self.class_name.as_ptr()), self.hinst);
        }
    }
}

fn assert_affinity(hwnd: HWND, expected: WINDOW_DISPLAY_AFFINITY, ctx: &str) {
    let got = current_display_affinity(hwnd)
        .unwrap_or_else(|| panic!("{ctx}: GetWindowDisplayAffinity failed"));
    assert_eq!(
        got.0, expected.0,
        "{ctx}: affinity mismatch (got {}, expected {})",
        got.0, expected.0
    );
}

#[test]
fn a_apply_sets_affinity_on_top_level_window() {
    let tree = Tree::new();
    let effective = apply_display_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE)
        .expect("apply_display_affinity");
    assert_eq!(
        effective.0, WDA_EXCLUDEFROMCAPTURE.0,
        "no fallback expected on a plain Win32 window"
    );
    assert_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE, "parent");
}

#[test]
fn b_apply_toggles_back_to_none() {
    let tree = Tree::new();
    apply_display_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE).unwrap();
    apply_display_affinity(tree.parent, WDA_NONE).unwrap();
    assert_affinity(tree.parent, WDA_NONE, "parent");
}

#[test]
fn c_apply_is_idempotent() {
    let tree = Tree::new();
    let r1 = apply_display_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE).unwrap();
    let r2 = apply_display_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE).unwrap();
    assert_eq!(
        r1.0, r2.0,
        "second apply must report the same effective affinity"
    );
    assert_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE, "parent after 2x");
}

#[test]
fn d_null_hwnd_returns_err_without_panic() {
    let res = apply_display_affinity(HWND::default(), WDA_EXCLUDEFROMCAPTURE);
    assert!(res.is_err(), "null HWND must return Err, got {res:?}");
}

#[test]
fn e_child_hwnd_is_rejected_by_display_affinity_apis() {
    let tree = Tree::new();

    // Documented: "A handle to the top-level window ... returns FALSE when,
    // for example, the function call is made on a non top-level window."
    let set = unsafe { SetWindowDisplayAffinity(tree.child, WDA_EXCLUDEFROMCAPTURE) };
    assert!(
        set.is_err(),
        "SetWindowDisplayAffinity must reject a WS_CHILD window; a success here \
         means the OS contract changed and a descendant walk may be worth revisiting"
    );
    assert!(
        current_display_affinity(tree.child).is_none(),
        "GetWindowDisplayAffinity must reject a WS_CHILD window"
    );

    // The parent is unaffected by the failed child calls and still settable.
    apply_display_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE).unwrap();
    assert_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE, "parent");
}
