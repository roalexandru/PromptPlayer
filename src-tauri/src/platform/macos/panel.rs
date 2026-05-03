//! NSPanel configuration — the canonical Spotlight/Raycast/Hammerspoon recipe
//! for a borderless utility panel that:
//!
//! 1. Is a subclassed `NSPanel` overriding `-canBecomeKeyWindow` → `YES`.
//!    This is the trick that lets a `nonActivatingPanel` actually receive
//!    keystrokes — without it the panel can never be key, so keys flow to
//!    the underlying app no matter what other flags are set.
//! 2. Has `styleMask |= NonactivatingPanel` so the panel can be key without
//!    triggering app activation (which would switch Spaces).
//! 3. Has `collectionBehavior = CanJoinAllSpaces | FullScreenAuxiliary` so
//!    the panel surfaces on whatever Space is current (incl. fullscreen).
//! 4. Has `level = NSPopUpMenuWindowLevel` (101).
//! 5. Is shown via `makeKeyAndOrderFront(nil)`. **Never** call
//!    `NSApp.activate(ignoringOtherApps:)` — that's what was switching
//!    Spaces on every previous attempt. With `.accessory` activation policy
//!    + the space-neutral library window (see `make_window_space_neutral`),
//!    activation does not switch Spaces, so we activate the app for the
//!    picker (it needs key/focus) but not for the popover.

use cocoa::base::id;
use objc::runtime::Class;
use objc::{class, msg_send, sel, sel_impl};
use std::sync::OnceLock;

extern "C" {
    fn object_setClass(obj: id, cls: *const Class) -> *const Class;
}

/// Configuration for an NSPanel that's about to be installed onto a Tauri
/// `WebviewWindow`. `Default` matches what the picker needs; the popover
/// uses a slightly different combination (no `can_become_key`, different
/// collection-behavior bitset).
pub struct PanelOptions {
    /// Subclass NSPanel with a runtime class overriding `canBecomeKeyWindow`
    /// → YES. Required for keyboard-input-receiving panels (the picker).
    pub can_become_key: bool,
    /// Bit-OR of `NSWindowCollectionBehavior*` flags.
    pub collection_behavior: u64,
    /// `NSWindowLevel` value. NSPopUpMenuWindowLevel = 101 is what native
    /// menu-bar popovers use.
    pub level: i64,
    /// Set `setAcceptsMouseMovedEvents:YES` on the panel so the WKWebView
    /// receives mouse-moved events (needed for hover state).
    pub accepts_mouse_moved: bool,
    /// Set `setBecomesKeyOnlyIfNeeded:NO` so the panel takes key on show
    /// (otherwise Esc never reaches the webview's keydown listener).
    pub becomes_key_on_show: bool,
}

impl PanelOptions {
    /// Picker preset: keyboard-input panel, joins all spaces (incl.
    /// fullscreen), at popup-menu level.
    pub const PICKER: PanelOptions = PanelOptions {
        can_become_key: true,
        collection_behavior: COLLECTION_CAN_JOIN_ALL_SPACES | COLLECTION_FULL_SCREEN_AUXILIARY,
        level: 101,
        accepts_mouse_moved: false,
        becomes_key_on_show: true,
    };

    /// Tray-popover preset: WiFi-menu-style popover that surfaces over
    /// fullscreen apps; receives mouse-moved events for hover state.
    pub const POPOVER: PanelOptions = PanelOptions {
        can_become_key: false,
        collection_behavior: COLLECTION_MOVE_TO_ACTIVE_SPACE | COLLECTION_FULL_SCREEN_AUXILIARY,
        level: 101,
        accepts_mouse_moved: true,
        becomes_key_on_show: true,
    };
}

const STYLE_MASK_NONACTIVATING_PANEL: u64 = 1 << 7;
const COLLECTION_CAN_JOIN_ALL_SPACES: u64 = 1 << 0;
const COLLECTION_MOVE_TO_ACTIVE_SPACE: u64 = 1 << 1;
const COLLECTION_FULL_SCREEN_AUXILIARY: u64 = 1 << 8;

/// Apply the picker preset to the Tauri picker window.
pub fn configure_picker_window(window: &tauri::WebviewWindow) {
    apply_options(window, &PanelOptions::PICKER);
}

/// Apply the popover preset to the tray-popup window.
pub fn configure_popover_window(window: &tauri::WebviewWindow) {
    apply_options(window, &PanelOptions::POPOVER);
}

fn apply_options(window: &tauri::WebviewWindow, opts: &PanelOptions) {
    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    unsafe {
        let ns_window: id = ns_window_ptr as id;

        // Re-class the window to NSPanel (or our `canBecomeKey` subclass).
        // After this the same object responds to NSPanel-only behaviors
        // and the NonactivatingPanel style flag becomes effective.
        let target_cls = if opts.can_become_key {
            register_picker_panel_class()
        } else {
            class!(NSPanel) as *const Class
        };
        object_setClass(ns_window, target_cls);

        // Enable nonactivating-panel style mask.
        let current_mask: u64 = msg_send![ns_window, styleMask];
        let _: () =
            msg_send![ns_window, setStyleMask: current_mask | STYLE_MASK_NONACTIVATING_PANEL];

        let _: () = msg_send![ns_window, setCollectionBehavior: opts.collection_behavior];
        let _: () = msg_send![ns_window, setLevel: opts.level];

        if opts.becomes_key_on_show {
            let _: () = msg_send![ns_window, setBecomesKeyOnlyIfNeeded: 0u8];
        }
        let _: () = msg_send![ns_window, setFloatingPanel: 1u8];
        let _: () = msg_send![ns_window, setHidesOnDeactivate: 0u8];
        let _: () = msg_send![ns_window, setHasShadow: 1u8];

        if opts.accepts_mouse_moved {
            let _: () = msg_send![ns_window, setAcceptsMouseMovedEvents: 1u8];
            let _: () = msg_send![ns_window, setIgnoresMouseEvents: 0u8];
            let _: () = msg_send![ns_window, setExcludedFromWindowsMenu: 1u8];
            let _: () = msg_send![ns_window, setMovableByWindowBackground: 0u8];
            // Force tracking-area rebuild on display so the WKWebView sets
            // up its NSTrackingArea over the new bounds.
            let content_view: id = msg_send![ns_window, contentView];
            if content_view != cocoa::base::nil {
                let _: () = msg_send![content_view, setNeedsDisplay: 1u8];
            }
        }
    }
}

/// Apply `CanJoinAllSpaces | FullScreenAuxiliary` to the library / about
/// windows so when our app is activated, macOS surfaces the window on the
/// CURRENT Space instead of switching the user back to the Space where the
/// app was launched. Without this, regular `NSWindow`s on an `.accessory`
/// app get bound to the launch Space and look "invisible" when shown from
/// a tray-popover click on a different Space.
pub fn make_window_space_neutral(window: &tauri::WebviewWindow) {
    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    unsafe {
        let ns_window: id = ns_window_ptr as id;
        let collection: u64 = COLLECTION_CAN_JOIN_ALL_SPACES | COLLECTION_FULL_SCREEN_AUXILIARY;
        let _: () = msg_send![ns_window, setCollectionBehavior: collection];
    }
}

/// Force a regular (non-panel) window to the very front of the window
/// stack, regardless of who currently has key focus. Tauri's `show()` +
/// `set_focus()` sequence is enough when the app is the foreground app,
/// but on `.accessory` apps the OS sometimes refuses focus transfer and
/// the window stays buried behind whichever app the user clicked from.
/// `orderFrontRegardless` is the AppKit-blessed way out — it raises the
/// window's z-order without requiring activation. Used by `show_window`
/// after the standard `set_focus()` so library / about always surface.
pub fn order_window_front_regardless(window: &tauri::WebviewWindow) {
    let Ok(ns_window_ptr) = window.ns_window() else {
        return;
    };
    unsafe {
        let ns_window: id = ns_window_ptr as id;
        let _: () = msg_send![ns_window, orderFrontRegardless];
    }
}

// ---------------------------------------------------------------------------
// Runtime Obj-C class registration: `PromptPlayerKeyPanel` overrides
// `canBecomeKeyWindow` and `canBecomeMainWindow` to return YES.
// ---------------------------------------------------------------------------

/// Wrapper around a static `*const Class` so we can stash it in `OnceLock`.
struct PanelClass(*const Class);
// SAFETY: pointer is to an Obj-C class registered for the process lifetime.
// Class metadata is immutable after registration, so the pointer is safe to
// share across threads.
unsafe impl Send for PanelClass {}
unsafe impl Sync for PanelClass {}

static PANEL_CLASS: OnceLock<PanelClass> = OnceLock::new();

/// Register (or return cached) `PromptPlayerKeyPanel` Obj-C class.
fn register_picker_panel_class() -> *const Class {
    use objc::declare::ClassDecl;
    use objc::runtime::{Object, Sel, BOOL, YES};
    use objc::sel;

    extern "C" fn can_become_key(_this: &Object, _cmd: Sel) -> BOOL {
        YES
    }
    extern "C" fn can_become_main(_this: &Object, _cmd: Sel) -> BOOL {
        YES
    }

    PANEL_CLASS
        .get_or_init(|| unsafe {
            let superclass = class!(NSPanel);
            let mut decl = ClassDecl::new("PromptPlayerKeyPanel", superclass)
                .expect("register PromptPlayerKeyPanel");
            decl.add_method(
                sel!(canBecomeKeyWindow),
                can_become_key as extern "C" fn(&Object, Sel) -> BOOL,
            );
            decl.add_method(
                sel!(canBecomeMainWindow),
                can_become_main as extern "C" fn(&Object, Sel) -> BOOL,
            );
            PanelClass(decl.register())
        })
        .0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_preset_is_keyboard_panel() {
        let p = &PanelOptions::PICKER;
        assert!(p.can_become_key);
        assert_eq!(p.level, 101);
        assert!(p.collection_behavior & COLLECTION_CAN_JOIN_ALL_SPACES != 0);
        assert!(p.collection_behavior & COLLECTION_FULL_SCREEN_AUXILIARY != 0);
    }

    #[test]
    fn popover_preset_is_non_keyboard() {
        let p = &PanelOptions::POPOVER;
        assert!(!p.can_become_key);
        assert!(p.accepts_mouse_moved);
        // MoveToActiveSpace, not CanJoinAllSpaces — the popover anchors to
        // the active Space; CanJoinAllSpaces would make it appear duplicated
        // across Mission Control thumbnails.
        assert!(p.collection_behavior & COLLECTION_MOVE_TO_ACTIVE_SPACE != 0);
    }

    #[test]
    fn flags_distinct() {
        assert_ne!(
            COLLECTION_CAN_JOIN_ALL_SPACES,
            COLLECTION_MOVE_TO_ACTIVE_SPACE
        );
        assert_ne!(
            STYLE_MASK_NONACTIVATING_PANEL,
            COLLECTION_CAN_JOIN_ALL_SPACES
        );
    }
}
