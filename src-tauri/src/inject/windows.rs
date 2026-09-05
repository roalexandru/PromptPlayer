//! Windows keystroke synthesis. `enigo 0.2`'s `text()` mis-synthesizes single
//! ASCII chars, so `type_char` drives `SendInput` directly.
//!
//! `paste_via_clipboard` owns the clipboard + Ctrl+V dance.

use super::PasteError;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{GetLastError, GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_LMENU, VK_LSHIFT, VK_MENU,
    VK_RMENU, VK_RSHIFT, VK_SHIFT, VK_V,
};

// windows-rs 0.58 doesn't re-export `CF_UNICODETEXT` under our feature flags.
// The value is fixed by definition: 13.
const CF_UNICODETEXT: u32 = 13;

/// Type one char via `SendInput` + `KEYEVENTF_UNICODE`, as a down/up pair per
/// UTF-16 code unit (so non-BMP chars send two pairs).
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

/// Read the clipboard's CF_UNICODETEXT flavor, if any. Used to populate the
/// `$CLIPBOARD` placeholder / `clipboard` expression builtin at fire time.
pub(crate) fn read_clipboard_string() -> Option<String> {
    unsafe {
        let _guard = open_clipboard_retry().ok()?;
        let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
        if handle.is_invalid() {
            return None;
        }
        let hglobal = HGLOBAL(handle.0 as _);
        let ptr = GlobalLock(hglobal) as *const u16;
        if ptr.is_null() {
            return None;
        }
        // CF_UNICODETEXT is a null-terminated UTF-16 string.
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let s = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(hglobal);
        Some(s)
    }
}

// --------------- clipboard paste -----------------

/// Set the clipboard, Ctrl+V, wait, restore. Formats we can't safely snapshot
/// error out before anything is touched.
pub(super) fn paste_via_clipboard(body: &str) -> Result<(), PasteError> {
    // 1. Snapshot the existing clipboard formats.
    let saved = save_clipboard().map_err(PasteError::Clipboard)?;

    // 2. Set ours.
    if let Err(e) = set_unicode_clipboard(body) {
        let _ = restore_clipboard(&saved);
        return Err(PasteError::Clipboard(e));
    }

    // 3. Ctrl+V, after releasing held modifiers — a stray Shift makes this
    //    paste-without-formatting, and a held Alt makes V an accelerator.
    release_user_modifiers();
    if let Err(e) = synth_ctrl_v() {
        // Best-effort restore even on synth failure.
        let _ = restore_clipboard(&saved);
        return Err(PasteError::Injection(e));
    }

    // 4. Let the app consume the paste before restoring. Restoring early makes
    //    a loaded Electron app paste the user's previous (private) clipboard.
    std::thread::sleep(Duration::from_millis(250));

    // 5. Restore. On failure, leaving our text behind beats an empty clipboard.
    if let Err(e) = restore_clipboard(&saved) {
        tracing::warn!("clipboard restore failed: {}", e);
    }
    Ok(())
}

struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn open_clipboard_retry() -> Result<ClipboardGuard, String> {
    // The clipboard is a global single-owner mutex other apps hold briefly,
    // so retry for ~250ms.
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        if unsafe { OpenClipboard(HWND::default()).is_ok() } {
            return Ok(ClipboardGuard);
        }
        if Instant::now() >= deadline {
            return Err(format!("OpenClipboard failed: {:?}", unsafe {
                GetLastError()
            }));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct ClipboardSnapshot {
    formats: Vec<ClipboardFormatData>,
}

struct ClipboardFormatData {
    format: u32,
    bytes: Vec<u8>,
}

fn save_clipboard() -> Result<ClipboardSnapshot, String> {
    unsafe {
        let _guard = open_clipboard_retry()?;
        let mut formats = Vec::new();
        let mut format = EnumClipboardFormats(0);
        while format != 0 {
            let handle = GetClipboardData(format)
                .map_err(|_| format!("GetClipboardData({format}) failed: {:?}", GetLastError()))?;
            if handle.is_invalid() {
                return Err(format!("clipboard format {format} returned invalid handle"));
            }
            let hglobal = HGLOBAL(handle.0 as _);
            let size = GlobalSize(hglobal);
            if size == 0 {
                return Err(format!(
                    "clipboard format {format} is not backed by movable global memory"
                ));
            }
            let ptr = GlobalLock(hglobal) as *const u8;
            if ptr.is_null() {
                return Err(format!("GlobalLock({format}) returned null"));
            }
            let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
            let _ = GlobalUnlock(hglobal);
            formats.push(ClipboardFormatData { format, bytes });
            format = EnumClipboardFormats(format);
        }
        Ok(ClipboardSnapshot { formats })
    }
}

fn restore_clipboard(snapshot: &ClipboardSnapshot) -> Result<(), String> {
    unsafe {
        let _guard = open_clipboard_retry()?;
        let _ = EmptyClipboard();
        for item in &snapshot.formats {
            let hglobal = alloc_global_bytes(&item.bytes)?;
            let h = HANDLE(hglobal.0 as _);
            if SetClipboardData(item.format, h).is_err() {
                let _ = GlobalFree(hglobal);
                return Err(format!(
                    "SetClipboardData({}) failed: {:?}",
                    item.format,
                    GetLastError()
                ));
            }
            // Ownership transferred to the clipboard.
        }
        Ok(())
    }
}

fn set_unicode_clipboard(text: &str) -> Result<(), String> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len() * std::mem::size_of::<u16>();
    unsafe {
        let data = std::slice::from_raw_parts(wide.as_ptr() as *const u8, bytes);
        let hglobal = alloc_global_bytes(data)?;

        // We own hglobal until SetClipboardData succeeds, so every error path
        // below must GlobalFree or leak a multi-KB block per attempt.
        let _guard = match open_clipboard_retry() {
            Ok(guard) => guard,
            Err(e) => {
                let _ = GlobalFree(hglobal);
                return Err(e);
            }
        };
        let _ = EmptyClipboard();
        let h = HANDLE(hglobal.0 as _);
        let set_ok = SetClipboardData(CF_UNICODETEXT, h).is_ok();
        if !set_ok {
            // Ownership did NOT transfer — we still own the alloc.
            let _ = GlobalFree(hglobal);
            return Err(format!("SetClipboardData failed: {:?}", GetLastError()));
        }
        // On success, the system owns hglobal — don't free.
        Ok(())
    }
}

fn alloc_global_bytes(bytes: &[u8]) -> Result<HGLOBAL, String> {
    unsafe {
        let hglobal = match GlobalAlloc(GMEM_MOVEABLE, bytes.len()) {
            Ok(h) => h,
            Err(_) => return Err(format!("GlobalAlloc failed: {:?}", GetLastError())),
        };
        let dst = GlobalLock(hglobal) as *mut u8;
        if dst.is_null() {
            let _ = GlobalFree(hglobal);
            return Err("GlobalLock returned null".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        let _ = GlobalUnlock(hglobal);
        Ok(hglobal)
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

/// Release any modifiers the OS reports as down. A lingering Alt or Shift at
/// the moment we synthesize Ctrl+V silently kills the paste.
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
