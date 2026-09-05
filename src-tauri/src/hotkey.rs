//! Hotkey string normalization.
//!
//! Accepts user-authored forms (e.g. `cmd+shift+1`, `Cmd+Shift+1`,
//! `⌘⇧1`, `option+space`) and normalizes them to Tauri's canonical
//! `Shortcut::from_str` parser form (e.g. `CmdOrCtrl+Shift+Digit1`).

/// Normalize a user-authored hotkey string to Tauri's canonical form.
///
/// Recognized modifier aliases:
/// - `cmd | command | ⌘ | meta | super | win | windows` → `CmdOrCtrl`
/// - `ctrl | control | ⌃` → `Control`
/// - `shift | ⇧` → `Shift`
/// - `alt | option | opt | ⌥` → `Alt`
///
/// Recognized key aliases:
/// - Single ASCII letters → `Key{LETTER}` (uppercased)
/// - Single ASCII digits → `Digit{N}`
/// - `esc | escape` → `Escape`
/// - `enter | return` → `Enter`
/// - `space | spacebar` → `Space`
/// - `tab` → `Tab`
/// - `backspace` → `Backspace`
/// - `up | down | left | right` → `ArrowUp` etc.
///
/// Anything else is title-cased and passed through (so `Comma`, `Period`,
/// `F1..F12` etc. work without explicit mapping).
pub fn normalize(input: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for raw in input.split(['+', '-']) {
        let s = raw.trim().to_lowercase();
        let mapped = match s.as_str() {
            "cmd" | "command" | "⌘" | "meta" | "super" | "win" | "windows" => "CmdOrCtrl".into(),
            "ctrl" | "control" | "⌃" => "Control".into(),
            "shift" | "⇧" => "Shift".into(),
            "alt" | "option" | "opt" | "⌥" => "Alt".into(),
            "0" => "Digit0".into(),
            "1" => "Digit1".into(),
            "2" => "Digit2".into(),
            "3" => "Digit3".into(),
            "4" => "Digit4".into(),
            "5" => "Digit5".into(),
            "6" => "Digit6".into(),
            "7" => "Digit7".into(),
            "8" => "Digit8".into(),
            "9" => "Digit9".into(),
            "esc" | "escape" => "Escape".into(),
            "enter" | "return" => "Enter".into(),
            "space" | "spacebar" => "Space".into(),
            "tab" => "Tab".into(),
            "backspace" => "Backspace".into(),
            "up" | "arrowup" => "ArrowUp".into(),
            "down" | "arrowdown" => "ArrowDown".into(),
            "left" | "arrowleft" => "ArrowLeft".into(),
            "right" | "arrowright" => "ArrowRight".into(),
            "\\" => "Backslash".into(),
            "/" => "Slash".into(),
            "," => "Comma".into(),
            "." => "Period".into(),
            ";" => "Semicolon".into(),
            "'" => "Quote".into(),
            "`" => "Backquote".into(),
            other if other.len() == 1 && other.chars().next().unwrap().is_ascii_alphabetic() => {
                format!("Key{}", other.to_uppercase())
            }
            other => {
                // Already canonical (Comma, Period, F1-F12, etc.) — title-case
                // the first char so user-typed `f1` becomes `F1`.
                let mut chars = other.chars();
                match chars.next() {
                    Some(c) => {
                        format!("{}{}", c.to_uppercase().collect::<String>(), chars.as_str())
                    }
                    None => other.into(),
                }
            }
        };
        parts.push(mapped);
    }
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_shift_letter() {
        assert_eq!(normalize("cmd+shift+P"), "CmdOrCtrl+Shift+KeyP");
    }

    #[test]
    fn alt_cmd_backslash_palette_default() {
        assert_eq!(normalize("alt+cmd+\\"), "Alt+CmdOrCtrl+Backslash");
    }

    #[test]
    fn unicode_modifier_glyphs_accepted() {
        // Glyphs work when separated like ASCII modifiers.
        assert_eq!(normalize("⌘+⇧+1"), "CmdOrCtrl+Shift+Digit1");
        assert_eq!(normalize("⌥+⌘+\\"), "Alt+CmdOrCtrl+Backslash");
    }

    #[test]
    fn case_insensitive_modifiers() {
        assert_eq!(normalize("CTRL+SHIFT+a"), "Control+Shift+KeyA");
        assert_eq!(normalize("cmd+shift+a"), normalize("CMD+SHIFT+A"));
    }

    #[test]
    fn dash_separator_treated_like_plus() {
        assert_eq!(normalize("cmd-shift-1"), "CmdOrCtrl+Shift+Digit1");
    }

    #[test]
    fn special_keys_are_recognized() {
        assert_eq!(normalize("esc"), "Escape");
        assert_eq!(normalize("space"), "Space");
        assert_eq!(normalize("enter"), "Enter");
        assert_eq!(normalize("tab"), "Tab");
        assert_eq!(normalize("backspace"), "Backspace");
        assert_eq!(normalize("up"), "ArrowUp");
        assert_eq!(normalize("cmd+right"), "CmdOrCtrl+ArrowRight");
    }

    #[test]
    fn function_keys_passthrough_titlecased() {
        assert_eq!(normalize("cmd+f1"), "CmdOrCtrl+F1");
        assert_eq!(normalize("cmd+F12"), "CmdOrCtrl+F12");
    }

    #[test]
    fn output_passes_tauri_shortcut_parser() {
        // The actual contract: whatever `normalize` produces must be
        // parseable by Tauri's `Shortcut::from_str`.
        use std::str::FromStr;
        use tauri_plugin_global_shortcut::Shortcut;
        for input in [
            "cmd+shift+P",
            "alt+cmd+\\",
            "cmd+shift+1",
            "ctrl+shift+a",
            "cmd+f1",
            "cmd+space",
            "esc",
        ] {
            let n = normalize(input);
            assert!(
                Shortcut::from_str(&n).is_ok(),
                "normalize({input:?}) → {n:?} did not parse"
            );
        }
    }

    #[test]
    fn whitespace_around_parts_stripped() {
        assert_eq!(normalize(" cmd  +  shift +  1 "), "CmdOrCtrl+Shift+Digit1");
    }

    #[test]
    fn alt_aliases_consistent() {
        let a = normalize("alt+cmd+a");
        let b = normalize("opt+cmd+a");
        let c = normalize("option+cmd+a");
        let d = normalize("⌥+cmd+a");
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(c, d);
    }

    #[test]
    fn meta_super_alias_to_cmdorctrl() {
        assert_eq!(normalize("super+a"), "CmdOrCtrl+KeyA");
        assert_eq!(normalize("meta+a"), "CmdOrCtrl+KeyA");
        assert_eq!(normalize("win+a"), "CmdOrCtrl+KeyA");
    }
}
