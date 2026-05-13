//! Windows-specific keystroke synthesis.
//!
//! `enigo 0.2`'s `text()` on Windows mis-synthesizes single-char ASCII calls
//! (every char comes out as 'a' regardless of the input — symptom matches a
//! known scan-code-aliasing regression). We bypass it for `type_char` and
//! drive `SendInput` directly with `KEYEVENTF_UNICODE`, which is the same
//! primitive enigo *should* use for arbitrary text. Backspace / Enter still
//! go through enigo because those use `key()` with a virtual-key code, which
//! is the unaffected code path.
//!
//! For chars outside the BMP (emoji etc.), UTF-16 returns a surrogate pair;
//! we emit each code unit as its own `INPUT` so the OS reassembles them.
//!
//! `paste_via_clipboard` lives here too: it owns the OpenClipboard /
//! GlobalAlloc / SetClipboardData dance and synthesizes Ctrl+V via SendInput.

use super::PasteError;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{GetLastError, GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

// `CF_UNICODETEXT` isn't re-exported by windows-rs 0.58 under any of the
// feature flags we already pull in (it lived in `Win32_System_SystemServices`
// in older releases and was moved out before 0.58). The value is stable by
// definition — wParam=13 is the standard clipboard format identifier.
const CF_UNICODETEXT: u32 = 13;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_LMENU, VK_LSHIFT, VK_MENU,
    VK_RMENU, VK_RSHIFT, VK_SHIFT, VK_V,
};

/// Synthesize a single character into the focused window using `SendInput`
/// with `KEYEVENTF_UNICODE`. Sends key-down + key-up per UTF-16 code unit
/// (a surrogate pair for non-BMP chars produces two pairs of inputs).
pub(crate) fn type_char_unicode(c: char) {
    let mut buf = [0u16; 2];
    let units = c.encode_utf16(&mut buf);
    for &unit in units.iter() {
        send_unicode_unit(unit);
    }
}

fn send_unicode_unit(unit: u16) {
    let down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_UNICODE.0 | KEYEVENTF_KEYUP.0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [down, up];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

// --------------- clipboard paste -----------------

/// Set the system clipboard to `body` (CF_UNICODETEXT), synthesize Ctrl+V on
/// the foreground window, wait briefly for the paste to be consumed, and
/// restore the previous clipboard contents. The save/restore covers
/// CF_UNICODETEXT only — sufficient for ~all stealth-demo scenarios; rich
/// formats / images are not preserved.
pub(super) fn paste_via_clipboard(body: &str) -> Result<(), PasteError> {
    // 1. Snapshot the existing clipboard text (best-effort).
    let saved = save_unicode_clipboard();

    // 2. Set ours.
    if let Err(e) = set_unicode_clipboard(body) {
        // Restore whatever we snapshotted (could be None) and bail.
        if let Some(s) = saved {
            let _ = set_unicode_clipboard(&s);
        }
        return Err(PasteError::Clipboard(e));
    }

    // 3. Synthesize Ctrl+V. Release any user-held modifiers (Alt from
    //    the Alt+Enter shortcut path, stray Shift, etc.) first so we
    //    don't synthesize Ctrl+Shift+V (which is "paste without
    //    formatting" / a different binding in many apps) or fight a
    //    still-held Alt that would turn V into a menu accelerator.
    release_user_modifiers();
    if let Err(e) = synth_ctrl_v() {
        // Best-effort restore even on synth failure.
        if let Some(s) = saved {
            let _ = set_unicode_clipboard(&s);
        }
        return Err(PasteError::Injection(e));
    }

    // 4. Give the target app time to consume the paste from the clipboard
    //    BEFORE we restore. Apps read CF_UNICODETEXT on the WM_PASTE
    //    handler synchronously after Ctrl+V, but the keystroke itself is
    //    delivered asynchronously and the WM_KEYUP for Ctrl needs to
    //    drain too. 60ms is conservative; users won't notice this delay
    //    because the text is already on screen.
    std::thread::sleep(Duration::from_millis(60));

    // 5. Restore the original clipboard if we had something to put back.
    //    On failure we log and move on — leaving our text on the clipboard
    //    is the lesser evil compared to leaving the clipboard empty.
    if let Some(s) = saved {
        if let Err(e) = set_unicode_clipboard(&s) {
            tracing::warn!("clipboard restore failed: {}", e);
        }
    }
    Ok(())
}

fn open_clipboard_retry() -> Result<(), String> {
    // The clipboard is a global single-owner mutex; another app
    // (Spotify, browsers, password managers) can hold it briefly.
    // Retry for up to ~250ms.
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        let ok = unsafe { OpenClipboard(HWND::default()).is_ok() };
        if ok {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("OpenClipboard failed: {:?}", unsafe {
                GetLastError()
            }));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn save_unicode_clipboard() -> Option<String> {
    unsafe {
        if !IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
            return None;
        }
        if open_clipboard_retry().is_err() {
            return None;
        }
        let handle = match GetClipboardData(CF_UNICODETEXT) {
            Ok(h) if !h.is_invalid() => h,
            _ => {
                let _ = CloseClipboard();
                return None;
            }
        };
        let hglobal = HGLOBAL(handle.0 as _);
        let ptr = GlobalLock(hglobal) as *const u16;
        let result = if ptr.is_null() {
            None
        } else {
            // Walk until NUL.
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            Some(OsString::from_wide(slice).to_string_lossy().into_owned())
        };
        let _ = GlobalUnlock(hglobal);
        let _ = CloseClipboard();
        result
    }
}

fn set_unicode_clipboard(text: &str) -> Result<(), String> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len() * std::mem::size_of::<u16>();
    unsafe {
        // GlobalAlloc with GMEM_MOVEABLE because SetClipboardData takes
        // ownership of moveable memory.
        // GlobalAlloc returns Result<HGLOBAL>: Ok means the alloc
        // succeeded. HGLOBAL has no `is_invalid()` in this crate version,
        // and we rely on the Result itself to gate the success path.
        let hglobal = match GlobalAlloc(GMEM_MOVEABLE, bytes) {
            Ok(h) => h,
            Err(_) => return Err(format!("GlobalAlloc failed: {:?}", GetLastError())),
        };
        let dst = GlobalLock(hglobal) as *mut u16;
        if dst.is_null() {
            let _ = GlobalFree(hglobal);
            return Err("GlobalLock returned null".into());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        let _ = GlobalUnlock(hglobal);

        open_clipboard_retry()?;
        let _ = EmptyClipboard();
        let h = HANDLE(hglobal.0 as _);
        let set_ok = SetClipboardData(CF_UNICODETEXT, h).is_ok();
        let _ = CloseClipboard();
        if !set_ok {
            // Ownership did NOT transfer — we still own the alloc.
            let _ = GlobalFree(hglobal);
            return Err(format!("SetClipboardData failed: {:?}", GetLastError()));
        }
        // On success, the system owns hglobal — don't free.
        Ok(())
    }
}

fn synth_ctrl_v() -> Result<(), String> {
    let ctrl_down = vk_input(VK_CONTROL, false);
    let v_down = vk_input(VK_V, false);
    let v_up = vk_input(VK_V, true);
    let ctrl_up = vk_input(VK_CONTROL, true);
    let inputs = [ctrl_down, v_down, v_up, ctrl_up];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(format!(
            "SendInput sent {}/{} events; last error {:?}",
            sent,
            inputs.len(),
            unsafe { GetLastError() }
        ));
    }
    Ok(())
}

fn vk_input(vk: VIRTUAL_KEY, keyup: bool) -> INPUT {
    let flags = if keyup {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Release any modifier keys the OS currently reports as pressed. This is
/// defensive: the user's `Alt+Enter` (or `Enter` while still tapering off
/// `Shift` from a prior word) can leave a modifier physically down at the
/// moment we synthesize Ctrl+V. If we don't release it first, V becomes a
/// menu accelerator / sticky-key combo and the paste silently dies.
fn release_user_modifiers() {
    let candidates = [VK_MENU, VK_LMENU, VK_RMENU, VK_SHIFT, VK_LSHIFT, VK_RSHIFT];
    let mut inputs: Vec<INPUT> = Vec::with_capacity(candidates.len());
    for vk in candidates {
        let state = unsafe { GetAsyncKeyState(vk.0 as i32) };
        // High bit set ⇒ key is currently down.
        if (state as u16) & 0x8000 != 0 {
            inputs.push(vk_input(vk, true));
        }
    }
    if !inputs.is_empty() {
        unsafe {
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }
}
