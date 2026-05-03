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

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY,
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
