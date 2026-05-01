//! §5.6 — filter chain. Composable per-prompt transformations applied at fire time.
//!
//! Built-ins: lowercase, uppercase, capitalize, trim, strip-thinking-blocks,
//! markdown-to-plain, inject-typos, regex-replace.
//!
//! Custom TypeScript filters via `boa_engine` are added in Phase 8 (this module
//! exposes a `Filter::Custom` variant gated by config).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FilterSpec {
    Bare(String),
    WithArgs(std::collections::HashMap<String, serde_json::Value>),
}

/// Apply a chain of filter specs (the strings from `filters:` in YAML) to `text`.
pub fn apply_chain(text: &str, specs: &[String]) -> String {
    let mut current = text.to_string();
    for spec in specs {
        current = apply_one(&current, spec);
    }
    current
}

fn apply_one(text: &str, spec: &str) -> String {
    let trimmed = spec.trim();
    // Allow `name: arg` style.
    let (name, arg) = match trimmed.split_once(':') {
        Some((n, a)) => (n.trim(), Some(a.trim())),
        None => (trimmed, None),
    };
    match name {
        "lowercase" => text.to_lowercase(),
        "uppercase" => text.to_uppercase(),
        "capitalize" => capitalize(text),
        "trim" => text.trim().to_string(),
        "strip-thinking-blocks" => strip_thinking_blocks(text),
        "markdown-to-plain" => markdown_to_plain(text),
        "inject-typos" => inject_typos(text, arg),
        "regex-replace" => regex_replace(text, arg),
        other => {
            tracing::warn!("unknown filter {:?}", other);
            text.to_string()
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

/// Strip `<thinking>...</thinking>` blocks (and `<think>`, common LLM patterns).
fn strip_thinking_blocks(text: &str) -> String {
    let re = regex::Regex::new(r"(?is)<think(?:ing)?>.*?</think(?:ing)?>").unwrap();
    re.replace_all(text, "").trim().to_string()
}

fn markdown_to_plain(text: &str) -> String {
    use pulldown_cmark::{Event, Parser, TagEnd};
    let mut out = String::new();
    for event in Parser::new(text) {
        match event {
            Event::Text(t) | Event::Code(t) => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::End(TagEnd::Paragraph) | Event::End(TagEnd::Heading(_)) => out.push_str("\n\n"),
            _ => {}
        }
    }
    out.trim().to_string()
}

fn inject_typos(text: &str, _arg: Option<&str>) -> String {
    // The Phase 1 typer already injects typos at schedule time. The filter is
    // here for completeness; running it on the source text would be redundant
    // for the typing path, but useful if a filter chain wants to seed visible
    // typos for paste-mode (Alt+Enter).
    text.to_string()
}

fn regex_replace(text: &str, arg: Option<&str>) -> String {
    let Some(spec) = arg else {
        return text.to_string();
    };
    // Format: `s/pattern/replacement/flags`
    let parts: Vec<&str> = spec.splitn(4, '/').collect();
    if parts.len() < 3 || parts[0] != "s" {
        return text.to_string();
    }
    let pattern = parts[1];
    let replacement = parts[2];
    let flags = parts.get(3).copied().unwrap_or("");
    let mut re_str = String::new();
    if flags.contains('i') {
        re_str.push_str("(?i)");
    }
    re_str.push_str(pattern);
    let Ok(re) = regex::Regex::new(&re_str) else {
        return text.to_string();
    };
    if flags.contains('g') {
        re.replace_all(text, replacement).into_owned()
    } else {
        re.replace(text, replacement).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercase_uppercase() {
        assert_eq!(apply_chain("Hello", &["lowercase".into()]), "hello");
        assert_eq!(apply_chain("Hello", &["uppercase".into()]), "HELLO");
    }

    #[test]
    fn strip_thinking_removes_tags() {
        let s = strip_thinking_blocks("hello <thinking>internal</thinking> world");
        assert_eq!(s, "hello  world");
    }

    #[test]
    fn chain_applies_in_order() {
        let s = apply_chain(
            "<think>x</think>  Hello  ",
            &["strip-thinking-blocks".into(), "trim".into(), "lowercase".into()],
        );
        assert_eq!(s, "hello");
    }

    #[test]
    fn regex_replace_with_global() {
        let s = apply_chain(
            "foo bar foo",
            &["regex-replace: s/foo/baz/g".into()],
        );
        assert_eq!(s, "baz bar baz");
    }

    #[test]
    fn regex_replace_first_only_without_g() {
        let s = apply_chain(
            "foo bar foo",
            &["regex-replace: s/foo/baz/".into()],
        );
        assert_eq!(s, "baz bar foo");
    }

    #[test]
    fn regex_replace_case_insensitive_flag() {
        let s = apply_chain(
            "Foo FOO",
            &["regex-replace: s/foo/baz/gi".into()],
        );
        assert_eq!(s, "baz baz");
    }

    #[test]
    fn regex_replace_invalid_pattern_is_passthrough() {
        let s = apply_chain(
            "abc",
            &["regex-replace: s/[/X/g".into()],
        );
        assert_eq!(s, "abc");
    }

    #[test]
    fn regex_replace_malformed_spec_is_passthrough() {
        let s = apply_chain("abc", &["regex-replace: not-an-s-spec".into()]);
        assert_eq!(s, "abc");
    }

    #[test]
    fn capitalize_first_char_only() {
        assert_eq!(apply_chain("hello world", &["capitalize".into()]), "Hello world");
        assert_eq!(apply_chain("", &["capitalize".into()]), "");
    }

    #[test]
    fn trim_strips_leading_trailing_whitespace() {
        assert_eq!(apply_chain("  hi  ", &["trim".into()]), "hi");
    }

    #[test]
    fn strip_thinking_handles_self_closed_thinking_tag() {
        // <think>...</think> alias also works (common LLM pattern).
        let s = strip_thinking_blocks("a<think>secret</think>b");
        assert_eq!(s, "ab");
    }

    #[test]
    fn strip_thinking_preserves_normal_text() {
        let s = strip_thinking_blocks("plain text without tags");
        assert_eq!(s, "plain text without tags");
    }

    #[test]
    fn markdown_to_plain_extracts_text() {
        let s = apply_chain(
            "# Heading\n\nSome **bold** text and `code`.",
            &["markdown-to-plain".into()],
        );
        assert!(s.contains("Heading"));
        assert!(s.contains("bold"));
        assert!(s.contains("code"));
        // Markdown emphasis markers should be gone.
        assert!(!s.contains("**"));
        assert!(!s.contains('`'));
    }

    #[test]
    fn unknown_filter_passes_through() {
        let s = apply_chain("untouched", &["this-filter-does-not-exist".into()]);
        assert_eq!(s, "untouched");
    }

    #[test]
    fn empty_chain_is_identity() {
        let s = apply_chain("hello", &[]);
        assert_eq!(s, "hello");
    }

    #[test]
    fn inject_typos_filter_is_currently_passthrough() {
        // Documented: typo injection happens at schedule time, not in the filter chain.
        // The filter is reserved for paste-mode scenarios.
        assert_eq!(apply_chain("hello", &["inject-typos".into()]), "hello");
    }

    #[test]
    fn filter_with_inline_arg_uses_colon_separator() {
        let s = apply_chain("AAAA", &["regex-replace: s/A/b/g".into()]);
        assert_eq!(s, "bbbb");
    }
}
