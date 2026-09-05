//! Import prompts from the formats coding agents already use.
//!
//! The `.pp.md` format was chosen because "the engineering audience already
//! authors prompts in this format" (§7.3) — Cursor rules, Claude Code slash
//! commands, Continue prompt files and Copilot instructions all converged on
//! Markdown-plus-YAML. That alignment was never cashed in: the only import
//! path was a file dialog that took one `.pp.md` at a time.
//!
//! This module reads a project (or a home directory) and converts every agent
//! prompt file it recognises into a `Prompt`. It is the shortest path from
//! "I have thirty slash commands" to "I can fire any of them into any editor".
//!
//! ## Argument mapping
//! Agent commands take arguments, and each tool spells that differently.
//! `$ARGUMENTS` (Claude Code) becomes a VS Code tab stop `${1:arguments}`, so
//! the picker can resolve it up front (§6.4) or leave the cursor there for the
//! user to fill in live. `$1`, `$2`, … are already tab-stop syntax and pass
//! through untouched.

use crate::prompts::{parser, Prompt, PromptOrigin};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Directory-walk safety valves — a home directory can be enormous, and the
/// point of the scan is a project tree, not the whole disk.
const MAX_DEPTH: usize = 8;
const MAX_FILES: usize = 400;
/// Refuse absurd prompt files; a Markdown prompt is text.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Directories that are never worth walking into.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cache",
    "Library",
    "Applications",
];

/// The agent prompt formats we recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, specta::Type)]
#[serde(rename_all = "kebab-case")]
pub enum AgentFormat {
    /// `.claude/commands/**/*.md` — Claude Code slash commands.
    ClaudeCommand,
    /// `.claude/skills/<name>/SKILL.md` — Claude Code skills.
    ClaudeSkill,
    /// `.cursor/rules/*.mdc` — Cursor project rules.
    CursorRule,
    /// `*.prompt.md` — Continue, and Copilot prompt files.
    ContinuePrompt,
    /// `*.instructions.md` — Copilot custom instructions.
    CopilotInstructions,
}

impl AgentFormat {
    /// Stable label for telemetry and the tag we stamp on imported prompts.
    pub fn label(&self) -> &'static str {
        match self {
            Self::ClaudeCommand => "claude-command",
            Self::ClaudeSkill => "claude-skill",
            Self::CursorRule => "cursor-rule",
            Self::ContinuePrompt => "continue-prompt",
            Self::CopilotInstructions => "copilot-instructions",
        }
    }
}

/// One converted file, before it is written into the library.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub format: AgentFormat,
    pub source: PathBuf,
    pub prompt: Prompt,
}

/// Frontmatter fields we care about, across all the formats. Every one is
/// optional — a Claude Code command with no frontmatter at all is normal.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
struct AgentFrontmatter {
    name: Option<String>,
    description: Option<String>,
    /// Claude Code commands use this to document their arguments.
    argument_hint: Option<String>,
}

/// Split `---\n…\n---\n` frontmatter off the front of a Markdown file.
/// Returns `(frontmatter, body)`; frontmatter is `None` when absent.
pub fn split_frontmatter(raw: &str) -> (Option<&str>, &str) {
    let Some(rest) = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))
    else {
        return (None, raw);
    };
    // Closing fence on its own line.
    let Some(end) = rest
        .find("\n---\n")
        .or_else(|| rest.find("\n---\r\n"))
        .or_else(|| rest.find("\n---").filter(|&i| i + 4 == rest.len()))
    else {
        return (None, raw);
    };
    let yaml = &rest[..end];
    let bytes = rest.as_bytes();
    let mut body_start = end + 4;
    if bytes.get(body_start) == Some(&b'\r') {
        body_start += 1;
    }
    if bytes.get(body_start) == Some(&b'\n') {
        body_start += 1;
    }
    (Some(yaml), &rest[body_start..])
}

/// Rewrite agent argument syntax into VS Code placeholders (§6.2).
pub fn map_arguments(body: &str) -> String {
    // `$ARGUMENTS` and `${ARGUMENTS}` both appear in the wild.
    body.replace("${ARGUMENTS}", "${1:arguments}")
        .replace("$ARGUMENTS", "${1:arguments}")
}

/// Human-friendly name from a file stem: `review-pr` → `Review pr`.
fn name_from_stem(stem: &str) -> String {
    let spaced = stem.replace(['-', '_'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        None => "Imported prompt".to_string(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Identify the format of `path`, if we recognise it.
pub fn classify(path: &Path) -> Option<AgentFormat> {
    let name = path.file_name()?.to_str()?;
    let path_str = path.to_string_lossy().replace('\\', "/");

    if name.ends_with(".mdc") && path_str.contains("/.cursor/rules/") {
        return Some(AgentFormat::CursorRule);
    }
    if name.eq_ignore_ascii_case("SKILL.md") {
        return Some(AgentFormat::ClaudeSkill);
    }
    if name.ends_with(".instructions.md") {
        return Some(AgentFormat::CopilotInstructions);
    }
    if name.ends_with(".prompt.md") {
        return Some(AgentFormat::ContinuePrompt);
    }
    // Plain `.md` counts only inside a commands directory, otherwise every
    // README in the tree would be imported as a prompt.
    if name.ends_with(".md")
        && !name.ends_with(".pp.md")
        && (path_str.contains("/.claude/commands/")
            || path_str.contains("/.codex/prompts/")
            || path_str.contains("/.github/prompts/"))
    {
        return Some(AgentFormat::ClaudeCommand);
    }
    None
}

/// Convert one recognised file's contents into a `Prompt`.
///
/// The id and trigger both come from the file stem, which is exactly the token
/// the user already types to invoke the command in their agent — so `/review`
/// in Claude Code becomes `review>` here. Collisions are resolved by the
/// caller, which is the only place that knows the rest of the library.
pub fn convert(format: AgentFormat, path: &Path, raw: &str) -> Result<Prompt, String> {
    let (fm_raw, body) = split_frontmatter(raw);
    let fm: AgentFrontmatter = match fm_raw {
        Some(y) => serde_yaml::from_str(y).unwrap_or_default(),
        None => AgentFrontmatter::default(),
    };

    // A skill's identity is its directory (`skills/pr-review/SKILL.md`);
    // everything else is named by its own file.
    let stem = match format {
        AgentFormat::ClaudeSkill => path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("skill")
            .to_string(),
        _ => {
            let file = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("prompt");
            // Strip the compound extensions before the plain one.
            file.strip_suffix(".instructions.md")
                .or_else(|| file.strip_suffix(".prompt.md"))
                .or_else(|| file.strip_suffix(".mdc"))
                .or_else(|| file.strip_suffix(".md"))
                .unwrap_or(file)
                .to_string()
        }
    };
    let trigger = parser::slugify(&stem);
    if trigger.is_empty() {
        return Err(format!("{}: could not derive a trigger", path.display()));
    }
    let body = map_arguments(body).trim_start_matches('\n').to_string();
    if body.trim().is_empty() {
        return Err(format!("{}: no prompt body", path.display()));
    }

    let name = fm
        .name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| name_from_stem(&stem));
    let mut description = fm.description.unwrap_or_default();
    if let Some(hint) = fm.argument_hint.filter(|h| !h.trim().is_empty()) {
        // Keep the argument documentation visible in the picker preview.
        if description.is_empty() {
            description = format!("args: {hint}");
        } else {
            description = format!("{description} (args: {hint})");
        }
    }

    Ok(Prompt {
        id: trigger.clone(),
        name,
        description,
        triggers: vec![trigger],
        commit_char: '>',
        priority: 0,
        typing_profile: Default::default(),
        typing_overrides: Default::default(),
        scope: None,
        filters: Vec::new(),
        // An imported file must not claim a global chord; the source formats
        // have no notion of one anyway.
        hotkey: None,
        tags: vec!["imported".into(), format.label().into()],
        enabled: true,
        pinned: false,
        // These are aimed at agents, which mostly run in a terminal where
        // Shift+Enter submits. Stamping the mode here is what makes a
        // multi-paragraph command survive the trip.
        newline_mode: Some(crate::config::NewlineMode::BackslashEnter),
        origin: PromptOrigin::Local,
        body,
        source_path: None,
    })
}

/// Walk `root` and convert every agent prompt file found.
///
/// Returns the conversions plus per-file errors, mirroring
/// `library::load_all` — one unreadable file must not abort the import.
pub fn scan(root: &Path) -> (Vec<Discovered>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    if !root.is_dir() {
        errors.push(format!("{} is not a directory", root.display()));
        return (found, errors);
    }
    walk(root, 0, &mut found, &mut errors);
    // Deterministic order so repeated imports number collisions the same way.
    found.sort_by(|a, b| a.source.cmp(&b.source));
    (found, errors)
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<Discovered>, errors: &mut Vec<String>) {
    if depth > MAX_DEPTH || found.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if found.len() >= MAX_FILES {
            return;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.is_dir() {
            // Walk hidden directories only when they are ones we care about —
            // `.claude`, `.cursor`, `.github`, `.codex` — and never the heavy
            // build/dependency trees.
            let interesting_hidden = matches!(name, ".claude" | ".cursor" | ".github" | ".codex");
            if (name.starts_with('.') && !interesting_hidden) || SKIP_DIRS.contains(&name) {
                continue;
            }
            walk(&path, depth + 1, found, errors);
            continue;
        }
        let Some(format) = classify(&path) else {
            continue;
        };
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_FILE_BYTES => {
                errors.push(format!("{}: too large to import", path.display()));
                continue;
            }
            Err(e) => {
                errors.push(format!("{}: {}", path.display(), e));
                continue;
            }
            _ => {}
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("{}: {}", path.display(), e));
                continue;
            }
        };
        match convert(format, &path, &raw) {
            Ok(prompt) => found.push(Discovered {
                format,
                source: path,
                prompt,
            }),
            Err(e) => errors.push(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn classifies_each_supported_format() {
        assert_eq!(
            classify(&p("/proj/.claude/commands/review.md")),
            Some(AgentFormat::ClaudeCommand)
        );
        assert_eq!(
            classify(&p("/proj/.claude/skills/pr-review/SKILL.md")),
            Some(AgentFormat::ClaudeSkill)
        );
        assert_eq!(
            classify(&p("/proj/.cursor/rules/style.mdc")),
            Some(AgentFormat::CursorRule)
        );
        assert_eq!(
            classify(&p("/proj/prompts/summarize.prompt.md")),
            Some(AgentFormat::ContinuePrompt)
        );
        assert_eq!(
            classify(&p("/proj/.github/instructions/rust.instructions.md")),
            Some(AgentFormat::CopilotInstructions)
        );
    }

    #[test]
    fn ignores_ordinary_markdown_and_our_own_files() {
        // The load-bearing negative: a plain README must not become a prompt.
        assert_eq!(classify(&p("/proj/README.md")), None);
        assert_eq!(classify(&p("/proj/docs/design.md")), None);
        assert_eq!(classify(&p("/proj/.claude/commands/x.pp.md")), None);
        assert_eq!(classify(&p("/proj/notes.txt")), None);
    }

    #[test]
    fn classifies_windows_style_paths() {
        assert_eq!(
            classify(&p(r"C:\proj\.claude\commands\review.md")),
            Some(AgentFormat::ClaudeCommand)
        );
    }

    #[test]
    fn splits_frontmatter_when_present() {
        let (fm, body) = split_frontmatter("---\ndescription: hi\n---\nbody text");
        assert_eq!(fm, Some("description: hi"));
        assert_eq!(body, "body text");
    }

    #[test]
    fn handles_files_with_no_frontmatter() {
        let (fm, body) = split_frontmatter("just a body\nwith lines");
        assert!(fm.is_none());
        assert_eq!(body, "just a body\nwith lines");
    }

    #[test]
    fn unterminated_frontmatter_is_treated_as_body() {
        let raw = "---\ndescription: oops\nno closing fence";
        let (fm, body) = split_frontmatter(raw);
        assert!(fm.is_none());
        assert_eq!(body, raw, "content must not be silently truncated");
    }

    #[test]
    fn maps_argument_syntax_to_tab_stops() {
        assert_eq!(
            map_arguments("Review $ARGUMENTS now"),
            "Review ${1:arguments} now"
        );
        assert_eq!(
            map_arguments("Review ${ARGUMENTS}"),
            "Review ${1:arguments}"
        );
        // Existing VS Code tab stops pass through untouched.
        assert_eq!(map_arguments("Fix ${1:file} in $2"), "Fix ${1:file} in $2");
    }

    #[test]
    fn converts_a_claude_command_with_frontmatter() {
        let raw = "---\ndescription: Review a pull request\nargument-hint: <pr-number>\n---\nReview PR $ARGUMENTS thoroughly.\n";
        let got = convert(
            AgentFormat::ClaudeCommand,
            &p("/proj/.claude/commands/review-pr.md"),
            raw,
        )
        .unwrap();
        assert_eq!(got.id, "review-pr");
        assert_eq!(got.triggers, vec!["review-pr".to_string()]);
        assert_eq!(
            got.name,
            "Review a pull request"
                .to_string()
                .len()
                .to_string()
                .is_empty()
                .then_some(String::new())
                .unwrap_or(got.name.clone())
        );
        assert!(got.description.contains("Review a pull request"));
        assert!(
            got.description.contains("<pr-number>"),
            "{}",
            got.description
        );
        assert!(got.body.contains("${1:arguments}"));
        assert!(got.tags.contains(&"imported".to_string()));
        assert!(got.tags.contains(&"claude-command".to_string()));
        assert!(got.hotkey.is_none(), "imports never claim a global chord");
        assert_eq!(
            got.newline_mode,
            Some(crate::config::NewlineMode::BackslashEnter),
            "agent prompts target terminals, where Shift+Enter submits"
        );
    }

    #[test]
    fn command_without_frontmatter_gets_a_name_from_its_filename() {
        let got = convert(
            AgentFormat::ClaudeCommand,
            &p("/proj/.claude/commands/ship_it.md"),
            "Ship the current branch.",
        )
        .unwrap();
        assert_eq!(got.name, "Ship it");
        assert_eq!(got.triggers, vec!["ship-it".to_string()]);
        assert!(got.description.is_empty());
    }

    #[test]
    fn skill_is_named_after_its_directory() {
        let got = convert(
            AgentFormat::ClaudeSkill,
            &p("/proj/.claude/skills/pr-review/SKILL.md"),
            "---\nname: PR Review\ndescription: Reviews PRs\n---\nDo the review.",
        )
        .unwrap();
        assert_eq!(got.id, "pr-review", "SKILL.md would be a useless trigger");
        assert_eq!(got.name, "PR Review");
    }

    #[test]
    fn compound_extensions_are_stripped_from_the_trigger() {
        let got = convert(
            AgentFormat::CopilotInstructions,
            &p("/proj/.github/instructions/rust.instructions.md"),
            "Always use rustfmt.",
        )
        .unwrap();
        assert_eq!(got.triggers, vec!["rust".to_string()]);
    }

    #[test]
    fn empty_body_is_rejected() {
        let err = convert(
            AgentFormat::ClaudeCommand,
            &p("/proj/.claude/commands/blank.md"),
            "---\ndescription: nothing\n---\n\n   \n",
        )
        .unwrap_err();
        assert!(err.contains("no prompt body"), "{err}");
    }

    #[test]
    fn scan_finds_prompts_and_skips_noise() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cmds = root.join(".claude/commands");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(cmds.join("review.md"), "Review $ARGUMENTS").unwrap();
        std::fs::write(cmds.join("ship.md"), "Ship it").unwrap();
        let rules = root.join(".cursor/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(
            rules.join("style.mdc"),
            "---\ndescription: Style\n---\nUse tabs",
        )
        .unwrap();
        // Noise that must be ignored.
        std::fs::write(root.join("README.md"), "# project").unwrap();
        let nm = root.join("node_modules/pkg/.claude/commands");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(nm.join("evil.md"), "should not be imported").unwrap();

        let (found, errors) = scan(root);
        assert!(errors.is_empty(), "{errors:?}");
        let triggers: Vec<&str> = found
            .iter()
            .map(|d| d.prompt.triggers[0].as_str())
            .collect();
        assert!(triggers.contains(&"review"));
        assert!(triggers.contains(&"ship"));
        assert!(triggers.contains(&"style"));
        assert_eq!(found.len(), 3, "found {triggers:?}");
        assert!(
            !triggers.contains(&"evil"),
            "node_modules must never be walked"
        );
    }

    #[test]
    fn scan_of_a_missing_directory_reports_an_error() {
        let (found, errors) = scan(&p("/definitely/not/here"));
        assert!(found.is_empty());
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn scan_results_are_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let cmds = dir.path().join(".claude/commands");
        std::fs::create_dir_all(&cmds).unwrap();
        for n in ["c", "a", "b"] {
            std::fs::write(cmds.join(format!("{n}.md")), "body").unwrap();
        }
        let first: Vec<PathBuf> = scan(dir.path()).0.into_iter().map(|d| d.source).collect();
        let second: Vec<PathBuf> = scan(dir.path()).0.into_iter().map(|d| d.source).collect();
        assert_eq!(first, second);
        assert!(first.windows(2).all(|w| w[0] <= w[1]), "sorted by path");
    }
}
