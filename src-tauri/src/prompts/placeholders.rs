//! §6.2 — VS Code-style placeholders: tab stops, the built-in variables listed
//! in `builtin_var`, and `${VAR/regex/repl/flags}` with case modifiers.
//!
//! Choices `${1|a,b,c|}` are recognized here but resolved by the picker, since
//! §6.4 rules out modal popups.

use chrono::Local;
use serde::Serialize;
use std::collections::HashMap;

/// Context supplied at fire time.
#[derive(Debug, Clone, Default)]
pub struct PlaceholderContext {
    pub clipboard: Option<String>,
    pub selection: Option<String>,
    pub app_name: Option<String>,
    pub app_bundle: Option<String>,
    pub window_title: Option<String>,
    pub tm_filename: Option<String>,
    /// Repo context for agent-companion prompts. Populated only when the body
    /// references one of them (see `fire.rs`), because resolving a repo means
    /// touching the filesystem.
    pub git_branch: Option<String>,
    pub repo_name: Option<String>,
    pub repo_root: Option<String>,
    /// Pre-resolved tab-stop / choice answers, keyed by stop index ("1", "2", ...).
    pub stop_answers: HashMap<String, String>,
    /// Optional override for randomness (testing).
    pub random_seed: Option<u64>,
}

/// Result of expanding a body — includes the final text + which tab stops remain unfilled.
#[derive(Debug, Clone, Serialize)]
pub struct Expansion {
    pub text: String,
    /// Tab stops the user did not pre-fill (Phase 6 picker will surface these).
    pub unfilled_stops: Vec<String>,
    /// `$0` cursor position, in char offset of `text`. None if absent.
    pub final_cursor: Option<usize>,
}

/// Expand placeholders in `body` using `ctx`. Phase 5 renders eagerly; Phase 8
/// adds lazy evaluation for `${{ ... }}` expressions.
pub fn expand(body: &str, ctx: &PlaceholderContext) -> Expansion {
    let mut out = String::with_capacity(body.len());
    let mut unfilled: Vec<String> = Vec::new();
    let mut final_cursor: Option<usize> = None;
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '$' {
            // Try `${...}` first.
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                if let Some(end) = find_matching_brace(&chars, i + 1) {
                    let inner: String = chars[i + 2..end].iter().collect();
                    let rendered = render_brace(&inner, ctx, &mut unfilled);
                    if let Some(stop_marker) = is_final_cursor(&inner) {
                        if stop_marker {
                            final_cursor = Some(out.chars().count());
                        }
                    }
                    out.push_str(&rendered);
                    i = end + 1;
                    continue;
                }
            }
            // Then bare `$NAME` or `$N`.
            let (name, advance) = read_bare(&chars, i + 1);
            if !name.is_empty() {
                let rendered = render_bare(&name, ctx, &mut unfilled);
                if name == "0" {
                    final_cursor = Some(out.chars().count());
                }
                out.push_str(&rendered);
                i += 1 + advance;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    Expansion {
        text: out,
        unfilled_stops: unfilled,
        final_cursor,
    }
}

fn read_bare(chars: &[char], start: usize) -> (String, usize) {
    let mut name = String::new();
    let mut j = start;
    if j < chars.len() && chars[j].is_ascii_digit() {
        while j < chars.len() && chars[j].is_ascii_digit() {
            name.push(chars[j]);
            j += 1;
        }
    } else {
        while j < chars.len()
            && (chars[j].is_ascii_alphabetic() || chars[j] == '_' || chars[j].is_ascii_digit())
        {
            name.push(chars[j]);
            j += 1;
        }
    }
    (name, j - start)
}

fn find_matching_brace(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open;
    while i < chars.len() {
        match chars[i] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn is_final_cursor(inner: &str) -> Option<bool> {
    Some(inner.trim() == "0")
}

fn render_brace(inner: &str, ctx: &PlaceholderContext, unfilled: &mut Vec<String>) -> String {
    let inner = inner.trim();

    // Find the first top-level separator (`:`, `/`, or `|`) that isn't inside
    // a nested `${...}`.
    let sep = find_top_level_separator(inner);

    match sep {
        Some((pos, '|')) if inner.ends_with('|') => {
            // Choice `1|a,b,c|`
            let key = inner[..pos].trim().to_string();
            if let Some(v) = ctx.stop_answers.get(&key) {
                return v.clone();
            }
            unfilled.push(key);
            let opts = &inner[pos + 1..inner.len() - 1];
            opts.split(',').next().unwrap_or("").to_string()
        }
        Some((pos, '/')) => {
            // Transformation `VAR/regex/repl/flags`
            let name = inner[..pos].trim().to_string();
            let rest = &inner[pos + 1..];
            let value = lookup_builtin(&name, ctx)
                .or_else(|| ctx.stop_answers.get(&name).cloned())
                .unwrap_or_default();
            apply_transform(&value, rest)
        }
        Some((pos, ':')) => {
            // Default value `1:default` or `NAME:default`
            let name = inner[..pos].trim().to_string();
            let default = inner[pos + 1..].to_string();
            if let Some(v) = ctx.stop_answers.get(&name) {
                return v.clone();
            }
            if name.chars().all(|c| c.is_ascii_digit()) {
                unfilled.push(name);
                return default;
            }
            if let Some(v) = lookup_builtin(&name, ctx) {
                return v;
            }
            default
        }
        _ => {
            // Bare tab stop or built-in.
            if inner.chars().all(|c| c.is_ascii_digit()) {
                if let Some(v) = ctx.stop_answers.get(inner) {
                    return v.clone();
                }
                if inner != "0" {
                    unfilled.push(inner.to_string());
                }
                return String::new();
            }
            lookup_builtin(inner, ctx).unwrap_or_default()
        }
    }
}

/// Split `s` on `delim` at top brace depth, up to `max` parts (last absorbs the rest).
fn split_top_level(s: &str, delim: char, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '$' && i + 1 < chars.len() && chars[i + 1] == '{' {
            depth += 1;
            current.push(c);
            current.push('{');
            i += 2;
            continue;
        }
        if c == '{' {
            depth += 1;
            current.push(c);
            i += 1;
            continue;
        }
        if c == '}' {
            depth -= 1;
            current.push(c);
            i += 1;
            continue;
        }
        if depth == 0 && c == delim && out.len() + 1 < max {
            out.push(std::mem::take(&mut current));
            i += 1;
            continue;
        }
        current.push(c);
        i += 1;
    }
    out.push(current);
    out
}

/// One user-answerable slot in a prompt body.
///
/// §6.4 is emphatic that a modal popup mid-expansion is a flow-killer, and
/// that choices should "resolve via the picker UI itself before the picker
/// dismisses". The expander already reported *which* stops were unfilled, but
/// not what they offer — so the picker had nothing to render and every choice
/// silently typed its first option. This is the missing half.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct PromptStop {
    /// Tab-stop index as authored (`"1"`, `"2"`, …).
    pub key: String,
    /// Options for a choice stop `${1|a,b,c|}`. Empty for a free-text stop.
    pub options: Vec<String>,
    /// Default text for `${1:default}`. `None` for a bare `$1`.
    pub default: Option<String>,
}

impl PromptStop {
    pub fn is_choice(&self) -> bool {
        !self.options.is_empty()
    }
}

/// Find every answerable tab stop / choice in `body`, in first-appearance
/// order, de-duplicated by key.
///
/// `$0` is excluded: it marks the final cursor position, not an input.
/// Built-in variables (`$CLIPBOARD`, `$DATE`, …) are excluded too — they
/// resolve from context, and asking the user for them would be nonsense.
pub fn scan_stops(body: &str) -> Vec<PromptStop> {
    let chars: Vec<char> = body.chars().collect();
    let mut out: Vec<PromptStop> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '$' {
            i += 1;
            continue;
        }
        // `${...}` form.
        if i + 1 < chars.len() && chars[i + 1] == '{' {
            let Some(end) = find_matching_brace(&chars, i + 1) else {
                i += 1;
                continue;
            };
            let inner: String = chars[i + 2..end].iter().collect();
            if let Some(stop) = parse_stop(&inner) {
                push_unique(&mut out, stop);
            }
            i = end + 1;
            continue;
        }
        // Bare `$N` form.
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        if j > i + 1 {
            let key: String = chars[i + 1..j].iter().collect();
            if key != "0" {
                push_unique(
                    &mut out,
                    PromptStop {
                        key,
                        options: Vec::new(),
                        default: None,
                    },
                );
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

fn push_unique(out: &mut Vec<PromptStop>, stop: PromptStop) {
    if let Some(existing) = out.iter_mut().find(|s| s.key == stop.key) {
        // A key can appear twice — `${1|a,b|}` then a bare `$1`. Keep the
        // richest description of it so the picker can still offer the choice.
        if existing.options.is_empty() && !stop.options.is_empty() {
            existing.options = stop.options;
        }
        if existing.default.is_none() {
            existing.default = stop.default;
        }
        return;
    }
    out.push(stop);
}

/// Parse the inside of one `${...}` into a stop, or `None` when it isn't one
/// (a transformation, or a built-in variable reference).
fn parse_stop(inner: &str) -> Option<PromptStop> {
    let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    match find_top_level_separator(inner) {
        Some((pos, '|')) if inner.ends_with('|') => {
            let key = inner[..pos].trim().to_string();
            if !numeric(&key) || key == "0" {
                return None;
            }
            let options = inner[pos + 1..inner.len() - 1]
                .split(',')
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty())
                .collect();
            Some(PromptStop {
                key,
                options,
                default: None,
            })
        }
        Some((pos, ':')) => {
            let key = inner[..pos].trim().to_string();
            if !numeric(&key) || key == "0" {
                return None;
            }
            Some(PromptStop {
                key,
                options: Vec::new(),
                default: Some(inner[pos + 1..].to_string()),
            })
        }
        // A `/` separator is a transformation (`${VAR/re/repl/}`), not a stop.
        Some((_, '/')) => None,
        _ => {
            let key = inner.trim().to_string();
            (numeric(&key) && key != "0").then_some(PromptStop {
                key,
                options: Vec::new(),
                default: None,
            })
        }
    }
}

/// Locate the first `:`, `/`, or `|` that appears at brace-depth 0
/// (i.e., not inside a nested `${...}`). Returns (position, char).
fn find_top_level_separator(s: &str) -> Option<(usize, char)> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '$' && i + 1 < bytes.len() && bytes[i + 1] as char == '{' {
            depth += 1;
            i += 2;
            continue;
        }
        if c == '{' {
            depth += 1;
            i += 1;
            continue;
        }
        if c == '}' {
            depth -= 1;
            i += 1;
            continue;
        }
        if depth == 0 && (c == ':' || c == '/' || c == '|') {
            return Some((i, c));
        }
        i += 1;
    }
    None
}

fn render_bare(name: &str, ctx: &PlaceholderContext, unfilled: &mut Vec<String>) -> String {
    if name.chars().all(|c| c.is_ascii_digit()) {
        if let Some(v) = ctx.stop_answers.get(name) {
            return v.clone();
        }
        if name != "0" {
            unfilled.push(name.to_string());
        }
        return String::new();
    }
    lookup_builtin(name, ctx).unwrap_or_default()
}

fn lookup_builtin(name: &str, ctx: &PlaceholderContext) -> Option<String> {
    let now = Local::now();
    Some(match name {
        "CLIPBOARD" => ctx.clipboard.clone().unwrap_or_default(),
        "SELECTION" => ctx.selection.clone().unwrap_or_default(),
        "APP_NAME" => ctx.app_name.clone().unwrap_or_default(),
        "APP_BUNDLE" => ctx.app_bundle.clone().unwrap_or_default(),
        "WINDOW_TITLE" => ctx.window_title.clone().unwrap_or_default(),
        "TM_FILENAME" => ctx.tm_filename.clone().unwrap_or_default(),
        // Agent-companion context (§ "Claude Code companion"): the repo the
        // user is demoing from, resolved without shelling out to git.
        "GIT_BRANCH" => ctx.git_branch.clone().unwrap_or_default(),
        "REPO_NAME" => ctx.repo_name.clone().unwrap_or_default(),
        "REPO_ROOT" | "CWD" => ctx.repo_root.clone().unwrap_or_default(),
        "USER" => std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default(),
        "MACHINE" => hostname().unwrap_or_default(),
        "DATE" => now.format("%Y-%m-%d").to_string(),
        "TIME" => now.format("%H:%M:%S").to_string(),
        "DATETIME" => now.to_rfc3339(),
        "UUID" => uuid::Uuid::new_v4().to_string(),
        "RANDOM" => {
            use rand::Rng;
            rand::thread_rng().gen::<u32>().to_string()
        }
        _ => return None,
    })
}

fn hostname() -> Option<String> {
    #[cfg(unix)]
    {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
    }
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").ok()
    }
}

/// `${VAR/regex/repl/flags}`, as a subset: group references and case modifiers
/// on the captured value. Full VS Code transform syntax is much larger.
fn apply_transform(value: &str, spec: &str) -> String {
    // spec format: `regex/repl/flags` — slashes inside nested `${...}` don't split.
    let parts = split_top_level(spec, '/', 3);
    if parts.len() < 2 {
        return value.to_string();
    }
    let pattern = parts[0].as_str();
    let repl = parts[1].as_str();
    let flags = parts.get(2).map(|s| s.as_str()).unwrap_or("");
    let global = flags.contains('g');
    let case_insensitive = flags.contains('i');

    let mut re_str = String::new();
    if case_insensitive {
        re_str.push_str("(?i)");
    }
    re_str.push_str(pattern);
    let Ok(re) = regex::Regex::new(&re_str) else {
        return value.to_string();
    };

    let replace_fn = |caps: &regex::Captures<'_>| -> String {
        let mut out = String::new();
        let mut chars = repl.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '$' {
                if let Some(&n) = chars.peek() {
                    if n == '{' {
                        chars.next();
                        let mut group = String::new();
                        while let Some(&p) = chars.peek() {
                            if p == '}' {
                                chars.next();
                                break;
                            }
                            group.push(p);
                            chars.next();
                        }
                        // group like "1:/upcase"
                        let (idx, modifier) = if let Some(colon) = group.find(':') {
                            (
                                group[..colon].to_string(),
                                Some(group[colon + 2..].to_string()),
                            )
                        } else {
                            (group, None)
                        };
                        let raw = idx
                            .parse::<usize>()
                            .ok()
                            .and_then(|n| caps.get(n))
                            .map(|m| m.as_str().to_string())
                            .unwrap_or_default();
                        let val = match modifier.as_deref() {
                            Some("upcase") => raw.to_uppercase(),
                            Some("downcase") => raw.to_lowercase(),
                            Some("capitalize") => capitalize(&raw),
                            Some("camelcase") => camel_case(&raw),
                            Some("pascalcase") => pascal_case(&raw),
                            Some("kebabcase") => kebab_case(&raw),
                            _ => raw,
                        };
                        out.push_str(&val);
                        continue;
                    }
                    if n.is_ascii_digit() {
                        let mut digits = String::new();
                        while let Some(&p) = chars.peek() {
                            if p.is_ascii_digit() {
                                digits.push(p);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                        if let Ok(idx) = digits.parse::<usize>() {
                            if let Some(m) = caps.get(idx) {
                                out.push_str(m.as_str());
                            }
                        }
                        continue;
                    }
                }
            }
            out.push(c);
        }
        out
    };

    if global {
        re.replace_all(value, replace_fn).into_owned()
    } else {
        re.replace(value, replace_fn).into_owned()
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

fn camel_case(s: &str) -> String {
    let words: Vec<&str> = s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i == 0 {
            out.push_str(&w.to_lowercase());
        } else {
            out.push_str(&capitalize(&w.to_lowercase()));
        }
    }
    out
}

fn pascal_case(s: &str) -> String {
    let words: Vec<&str> = s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut out = String::new();
    for w in words {
        out.push_str(&capitalize(&w.to_lowercase()));
    }
    out
}

fn kebab_case(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_a_choice_stop_with_its_options() {
        let stops = scan_stops("Style: ${1|aggressive,conservative|} please");
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].key, "1");
        assert_eq!(stops[0].options, vec!["aggressive", "conservative"]);
        assert!(stops[0].is_choice());
    }

    #[test]
    fn scans_a_default_stop() {
        let stops = scan_stops("Target ${2:main} branch");
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].key, "2");
        assert_eq!(stops[0].default.as_deref(), Some("main"));
        assert!(!stops[0].is_choice());
    }

    #[test]
    fn scans_bare_numeric_stops() {
        let stops = scan_stops("Fix $1 then $2");
        let keys: Vec<&str> = stops.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, vec!["1", "2"]);
    }

    #[test]
    fn excludes_the_final_cursor_marker() {
        // `$0` is where the cursor lands, not something to ask the user for.
        assert!(scan_stops("body text $0").is_empty());
        assert!(scan_stops("body ${0}").is_empty());
    }

    #[test]
    fn excludes_builtin_variables() {
        let body = "$CLIPBOARD $SELECTION ${DATE} $GIT_BRANCH ${TM_FILENAME}";
        assert!(scan_stops(body).is_empty(), "{:?}", scan_stops(body));
    }

    #[test]
    fn excludes_transformations() {
        // `${SELECTION/(.*)/${1:/pascalcase}/g}` is a transform, not an input.
        let stops = scan_stops("${SELECTION/(.*)/x/g}");
        assert!(stops.is_empty(), "{stops:?}");
    }

    #[test]
    fn deduplicates_a_key_and_keeps_the_richest_form() {
        // A choice declared once and referenced again must still be offered
        // as a choice.
        let stops = scan_stops("${1|a,b|} … and again $1");
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].options, vec!["a", "b"]);

        let stops = scan_stops("$1 … then ${1|a,b|}");
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].options, vec!["a", "b"]);
    }

    #[test]
    fn preserves_first_appearance_order() {
        let stops = scan_stops("${2:second} then ${1|x,y|}");
        let keys: Vec<&str> = stops.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, vec!["2", "1"]);
    }

    #[test]
    fn ignores_unterminated_braces() {
        // Must not panic or hang on malformed input.
        let _ = scan_stops("${1|a,b");
        let _ = scan_stops("${");
        let _ = scan_stops("$");
    }

    #[test]
    fn answers_from_the_picker_are_used_instead_of_defaults() {
        // The end-to-end contract behind the picker's choice resolver.
        let mut ctx = PlaceholderContext::default();
        ctx.stop_answers.insert("1".into(), "conservative".into());
        let out = expand("Style: ${1|aggressive,conservative|}", &ctx);
        assert_eq!(out.text, "Style: conservative");
        assert!(
            out.unfilled_stops.is_empty(),
            "an answered stop is not unfilled"
        );
    }

    #[test]
    fn an_unanswered_choice_still_falls_back_to_the_first_option() {
        let out = expand(
            "Style: ${1|aggressive,conservative|}",
            &PlaceholderContext::default(),
        );
        assert_eq!(out.text, "Style: aggressive");
        assert_eq!(out.unfilled_stops, vec!["1".to_string()]);
    }

    #[test]
    fn repo_context_placeholders_expand() {
        let ctx = PlaceholderContext {
            git_branch: Some("feature/x".into()),
            repo_name: Some("PromptPlayer".into()),
            repo_root: Some("/src/PromptPlayer".into()),
            ..Default::default()
        };
        let out = expand("On $GIT_BRANCH of ${REPO_NAME} at $CWD", &ctx);
        assert_eq!(
            out.text,
            "On feature/x of PromptPlayer at /src/PromptPlayer"
        );
    }

    #[test]
    fn missing_repo_context_expands_to_empty_not_literal() {
        let out = expand("[$GIT_BRANCH]", &PlaceholderContext::default());
        assert_eq!(out.text, "[]");
    }

    fn ctx_with_clipboard(c: &str) -> PlaceholderContext {
        PlaceholderContext {
            clipboard: Some(c.into()),
            ..Default::default()
        }
    }

    #[test]
    fn bare_clipboard_expands() {
        let e = expand("Hello $CLIPBOARD", &ctx_with_clipboard("World"));
        assert_eq!(e.text, "Hello World");
    }

    #[test]
    fn brace_clipboard_expands() {
        let e = expand("Hello ${CLIPBOARD}", &ctx_with_clipboard("World"));
        assert_eq!(e.text, "Hello World");
    }

    #[test]
    fn tab_stops_unfilled_have_empty_text() {
        let e = expand("a $1 b ${2:default} c", &PlaceholderContext::default());
        assert_eq!(e.text, "a  b default c");
        assert_eq!(e.unfilled_stops, vec!["1", "2"]);
    }

    #[test]
    fn final_cursor_recorded() {
        let e = expand("hello $0 world", &PlaceholderContext::default());
        assert_eq!(e.final_cursor, Some(6));
    }

    #[test]
    fn choice_renders_first_option() {
        let e = expand(
            "style: ${1|aggressive,conservative|}",
            &PlaceholderContext::default(),
        );
        assert_eq!(e.text, "style: aggressive");
        assert_eq!(e.unfilled_stops, vec!["1"]);
    }

    #[test]
    fn choice_uses_pre_filled_answer() {
        let mut ctx = PlaceholderContext::default();
        ctx.stop_answers.insert("1".into(), "conservative".into());
        let e = expand("style: ${1|aggressive,conservative|}", &ctx);
        assert_eq!(e.text, "style: conservative");
        assert!(e.unfilled_stops.is_empty());
    }

    #[test]
    fn pascal_case_transform_on_selection() {
        let ctx = PlaceholderContext {
            selection: Some("user-profile".into()),
            ..Default::default()
        };
        let e = expand("${SELECTION/(.*)/${1:/pascalcase}/g}", &ctx);
        assert_eq!(e.text, "UserProfile");
    }

    #[test]
    fn date_renders_iso() {
        let e = expand("today: $DATE", &PlaceholderContext::default());
        assert!(e.text.starts_with("today: 20"));
    }

    #[test]
    fn unknown_bare_var_renders_empty() {
        // Unknown variables should not crash; they render as empty.
        let e = expand("a $NOTAREALVAR b", &PlaceholderContext::default());
        // The exact rendering policy is "empty for unknown var", so the result
        // should be "a  b" or similar — assert it doesn't include the literal name.
        assert!(!e.text.contains("NOTAREALVAR"));
    }

    #[test]
    fn dollar_at_end_of_string_is_literal() {
        let e = expand("price: $", &PlaceholderContext::default());
        assert_eq!(e.text, "price: $");
    }

    #[test]
    fn nested_braces_are_handled() {
        // ${1/regex/${1:/upcase}/g} contains a nested ${1:/upcase}
        let ctx = PlaceholderContext {
            selection: Some("hello".into()),
            ..Default::default()
        };
        let e = expand("${SELECTION/(.*)/${1:/upcase}/g}", &ctx);
        assert_eq!(e.text, "HELLO");
    }

    #[test]
    fn multiple_tab_stops_collected() {
        let e = expand("$1 then $2 then $3", &PlaceholderContext::default());
        assert_eq!(e.unfilled_stops.len(), 3);
    }

    #[test]
    fn user_machine_render_non_empty() {
        // We don't assert the value (test machines vary) but they must expand.
        let e = expand("$USER@$MACHINE", &PlaceholderContext::default());
        // Should not include literal `$USER` after expansion.
        assert!(!e.text.contains("$USER"));
        assert!(!e.text.contains("$MACHINE"));
    }

    #[test]
    fn random_renders_digit() {
        let e = expand("$RANDOM", &PlaceholderContext::default());
        // Defaults to digits — must parse as a non-negative int.
        let n: u64 = e.text.parse().expect("RANDOM should be numeric");
        let _ = n;
    }

    #[test]
    fn uuid_is_36_chars() {
        let e = expand("$UUID", &PlaceholderContext::default());
        assert_eq!(e.text.len(), 36, "uuid v4 hyphenated form is 36 chars");
        assert_eq!(e.text.chars().filter(|c| *c == '-').count(), 4);
    }

    #[test]
    fn body_without_placeholders_unchanged() {
        let body = "this body has no placeholders, just text.";
        let e = expand(body, &PlaceholderContext::default());
        assert_eq!(e.text, body);
        assert!(e.unfilled_stops.is_empty());
        assert!(e.final_cursor.is_none());
    }
}
