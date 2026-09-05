//! T2 — recursive `SetWindowDisplayAffinity` integration tests.
//!
//! Builds a bare Win32 HWND tree (no Tauri / WebView2 dependencies) with the
//! shape that mimics how WebView2 hosts its swap chain:
//!
//! ```text
//!   parent (popup, off-screen, hidden)
//!   ├── child1 (WS_CHILD)
//!   │   └── grandchild (WS_CHILD)
//!   └── child2 (WS_CHILD)
//! ```
//!
//! Cases (matches the plan's T2 A–E):
//!  - **A** `apply_affinity_recursive(WDA_EXCLUDEFROMCAPTURE)` sets the flag
//!    on parent + both children + grandchild. The pre-Layer-B code only set
//!    it on the parent, so this is the load-bearing regression assertion.
//!  - **B** Toggling back with `WDA_NONE` reverts every descendant.
//!  - **C** Re-applying is idempotent.
//!  - **D** Destroying one child before the walk doesn't crash the function
//!    or stop it from applying to the rest.
//!  - **E** A null parent HWND returns `Err`, no panic.
//!
//! Why an integration test and not a unit test: this exercises real Win32
//! state (RegisterClass, CreateWindow, SetWindowDisplayAffinity / Get…),
//! which is heavier than fits inline in `capture.rs`'s `#[cfg(test)] mod`.
//! Integration-test placement avoids the `tauri/test` linkage issue that
//! gates `tests/ipc_registry.rs` off Windows — we deliberately do not import
//! `tauri` here.

#![cfg(target_os = "windows")]

use prompt_player::platform::windows::capture::{
    apply_affinity_recursive, current_display_affinity, enumerate_descendants,
};
use std::sync::atomic::{AtomicU32, Ordering};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, UnregisterClassW,
    WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WINDOW_DISPLAY_AFFINITY, WINDOW_EX_STYLE, WNDCLASSEXW,
    WNDCLASS_STYLES, WS_CHILD, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};

/// Monotonic counter so each test gets a unique class name. Class
/// registration is process-global; re-using a name across tests in the same
/// run would fail the second `RegisterClassExW`.
static CLASS_COUNTER: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// RAII scaffolding for the test tree. `Drop` destroys windows and
/// unregisters the class so leftover state from a panicking test doesn't
/// poison subsequent tests in the same binary.
struct Tree {
    parent: HWND,
    child1: HWND,
    child2: HWND,
    grandchild: HWND,
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

            let mk_child = |owner: HWND| -> HWND {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    PCWSTR(class_name.as_ptr()),
                    PCWSTR::null(),
                    WS_CHILD | WS_VISIBLE,
                    0,
                    0,
                    1,
                    1,
                    owner,
                    None,
                    hinst,
                    None,
                )
                .expect("CreateWindowExW child")
            };
            let child1 = mk_child(parent);
            let child2 = mk_child(parent);
            let grandchild = mk_child(child1);

            Tree {
                parent,
                child1,
                child2,
                grandchild,
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
fn a_recursive_apply_sets_affinity_on_full_tree() {
    let tree = Tree::new();

    // Sanity: EnumChildWindows surfaces all descendants, not just direct children.
    let descs = enumerate_descendants(tree.parent);
    let descs_addrs: Vec<usize> = descs.iter().map(|h| h.0 as usize).collect();
    assert!(
        descs_addrs.contains(&(tree.child1.0 as usize)),
        "descendants must include child1"
    );
    assert!(
        descs_addrs.contains(&(tree.child2.0 as usize)),
        "descendants must include child2"
    );
    assert!(
        descs_addrs.contains(&(tree.grandchild.0 as usize)),
        "descendants must include grandchild"
    );

    let (applied, attempted) = apply_affinity_recursive(tree.parent, WDA_EXCLUDEFROMCAPTURE)
        .expect("apply_affinity_recursive");
    assert_eq!(
        applied, attempted,
        "every Set call should succeed on valid HWNDs"
    );
    assert!(
        attempted >= 4,
        "expected parent + 3 descendants, got attempted={attempted}"
    );

    assert_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE, "parent");
    assert_affinity(tree.child1, WDA_EXCLUDEFROMCAPTURE, "child1");
    assert_affinity(tree.child2, WDA_EXCLUDEFROMCAPTURE, "child2");
    assert_affinity(tree.grandchild, WDA_EXCLUDEFROMCAPTURE, "grandchild");
}

#[test]
fn b_recursive_apply_toggles_back_to_none() {
    let tree = Tree::new();
    apply_affinity_recursive(tree.parent, WDA_EXCLUDEFROMCAPTURE).unwrap();
    apply_affinity_recursive(tree.parent, WDA_NONE).unwrap();

    assert_affinity(tree.parent, WDA_NONE, "parent");
    assert_affinity(tree.child1, WDA_NONE, "child1");
    assert_affinity(tree.child2, WDA_NONE, "child2");
    assert_affinity(tree.grandchild, WDA_NONE, "grandchild");
}

#[test]
fn c_recursive_apply_is_idempotent() {
    let tree = Tree::new();
    let r1 = apply_affinity_recursive(tree.parent, WDA_EXCLUDEFROMCAPTURE).unwrap();
    let r2 = apply_affinity_recursive(tree.parent, WDA_EXCLUDEFROMCAPTURE).unwrap();
    assert_eq!(
        r1, r2,
        "second apply must produce identical (applied, attempted) counts"
    );
    assert_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE, "parent after 2x");
    assert_affinity(
        tree.grandchild,
        WDA_EXCLUDEFROMCAPTURE,
        "grandchild after 2x",
    );
}

#[test]
fn d_destroyed_child_does_not_break_walk() {
    let tree = Tree::new();
    unsafe {
        let _ = DestroyWindow(tree.child2);
    }
    // EnumChildWindows skips the destroyed HWND; remaining tree must still
    // get the flag set. Function must not error out.
    let res = apply_affinity_recursive(tree.parent, WDA_EXCLUDEFROMCAPTURE);
    assert!(
        res.is_ok(),
        "walk must not error when a sibling was destroyed: {res:?}"
    );
    assert_affinity(tree.parent, WDA_EXCLUDEFROMCAPTURE, "parent");
    assert_affinity(tree.child1, WDA_EXCLUDEFROMCAPTURE, "child1");
    assert_affinity(tree.grandchild, WDA_EXCLUDEFROMCAPTURE, "grandchild");
}

#[test]
fn e_null_parent_returns_err_without_panic() {
    let res = apply_affinity_recursive(HWND::default(), WDA_EXCLUDEFROMCAPTURE);
    assert!(res.is_err(), "null parent must return Err, got {res:?}");
}
