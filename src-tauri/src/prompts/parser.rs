//! §7 — `.pp.md` parser: YAML frontmatter + Markdown body.

use crate::prompts::Prompt;
use crate::scopes::ScopeFilter;
use crate::typer::{ProfileKind, TypingOverrides};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("missing frontmatter delimiters in {path:?}")]
    MissingFrontmatter { path: PathBuf },
    #[error("invalid YAML in {path:?}: {source}")]
    Yaml {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("io error reading {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
struct Frontmatter {
    name: String,
    description: String,
    triggers: Vec<String>,
    commit_char: Option<String>,
    priority: Option<i32>,
    typing_profile: Option<ProfileKind>,
    typing_overrides: Option<TypingOverrides>,
    scope: Option<ScopeFilter>,
    filters: Vec<String>,
    hotkey: Option<String>,
    tags: Vec<String>,
    /// If unset, derived from the file name.
    id: Option<String>,
    enabled: Option<bool>,
    pinned: Option<bool>,
}

/// Parse a `.pp.md` file: leading `---\n<yaml>\n---\n<body>`.
pub fn parse_file(path: &Path) -> Result<Prompt, ParseError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ParseError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut prompt = parse_str(&raw, path)?;
    prompt.source_path = Some(path.to_path_buf());
    Ok(prompt)
}

pub fn parse_str(raw: &str, path: &Path) -> Result<Prompt, ParseError> {
    let stripped = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"));
    let Some(rest) = stripped else {
        return Err(ParseError::MissingFrontmatter {
            path: path.to_path_buf(),
        });
    };
    let end = rest
        .find("\n---\n")
        .or_else(|| rest.find("\n---\r\n"))
        .or_else(|| {
            // Trailing `\n---` with no following newline (file ends right after).
            rest.find("\n---").filter(|&i| i + 4 == rest.len())
        })
        .ok_or_else(|| ParseError::MissingFrontmatter {
            path: path.to_path_buf(),
        })?;
    let yaml = &rest[..end];
    // Skip past `\n---` (4 bytes), then optional `\r`, then optional `\n`.
    let bytes = rest.as_bytes();
    let mut body_start = end + 4;
    if bytes.get(body_start) == Some(&b'\r') {
        body_start += 1;
    }
    if bytes.get(body_start) == Some(&b'\n') {
        body_start += 1;
    }
    let body = rest[body_start..].to_string();

    let fm: Frontmatter = serde_yaml::from_str(yaml).map_err(|e| ParseError::Yaml {
        path: path.to_path_buf(),
        source: e,
    })?;

    let id = fm.id.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.trim_end_matches(".pp"))
            .unwrap_or("unnamed")
            .to_string()
    });
    let commit = fm
        .commit_char
        .as_deref()
        .and_then(|s| s.chars().next())
        .unwrap_or('>');

    Ok(Prompt {
        id,
        name: fm.name,
        description: fm.description,
        triggers: fm.triggers,
        commit_char: commit,
        priority: fm.priority.unwrap_or(0),
        typing_profile: fm.typing_profile.unwrap_or_default(),
        typing_overrides: fm.typing_overrides.unwrap_or_default(),
        scope: fm.scope,
        filters: fm.filters,
        hotkey: fm.hotkey,
        tags: fm.tags,
        enabled: fm.enabled.unwrap_or(true),
        pinned: fm.pinned.unwrap_or(false),
        body,
        source_path: None,
    })
}

/// Serialize a Prompt back to `.pp.md` form: YAML frontmatter + Markdown body.
pub fn serialize(prompt: &Prompt) -> Result<String, serde_yaml::Error> {
    use crate::typer::ProfileKind;
    // Build a serializable frontmatter mirror so we can emit kebab-case fields
    // and skip empties to keep files clean.
    #[derive(serde::Serialize)]
    #[serde(rename_all = "kebab-case")]
    struct Out<'a> {
        name: &'a str,
        #[serde(skip_serializing_if = "str::is_empty")]
        description: &'a str,
        triggers: &'a [String],
        commit_char: String,
        #[serde(skip_serializing_if = "is_zero")]
        priority: i32,
        typing_profile: ProfileKind,
        #[serde(skip_serializing_if = "is_default_overrides")]
        typing_overrides: &'a crate::typer::TypingOverrides,
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: &'a Option<crate::scopes::ScopeFilter>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        filters: &'a Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hotkey: &'a Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tags: &'a Vec<String>,
        #[serde(skip_serializing_if = "is_true")]
        enabled: bool,
        #[serde(skip_serializing_if = "is_false")]
        pinned: bool,
    }
    fn is_zero(v: &i32) -> bool {
        *v == 0
    }
    fn is_true(v: &bool) -> bool {
        *v
    }
    fn is_false(v: &bool) -> bool {
        !*v
    }
    fn is_default_overrides(v: &crate::typer::TypingOverrides) -> bool {
        v.iki_median_ms.is_none()
            && v.typo_rate.is_none()
            && v.pause_variance_scale.is_none()
            && v.burst_enabled.is_none()
            && v.typos_enabled.is_none()
            && v.pre_submit_pause_enabled.is_none()
            && v.send_final_enter.is_none()
    }

    let out = Out {
        name: &prompt.name,
        description: &prompt.description,
        triggers: &prompt.triggers,
        commit_char: prompt.commit_char.to_string(),
        priority: prompt.priority,
        typing_profile: prompt.typing_profile,
        typing_overrides: &prompt.typing_overrides,
        scope: &prompt.scope,
        filters: &prompt.filters,
        hotkey: &prompt.hotkey,
        tags: &prompt.tags,
        enabled: prompt.enabled,
        pinned: prompt.pinned,
    };
    let yaml = serde_yaml::to_string(&out)?;
    Ok(format!("---\n{yaml}---\n{}", prompt.body))
}

/// Slugify a string to be used as a filesystem name / prompt id.
pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("untitled");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic() {
        let raw = "---\nname: refactor-to-async\ndescription: Refactor a sync function\ntriggers: [refactor, refac, rfc]\ncommit-char: \">\"\npriority: 100\ntyping-profile: sales-engineer\ntags: [refactor, async]\n---\n\nRefactor this code.\n";
        let p = parse_str(raw, std::path::Path::new("refactor.pp.md")).unwrap();
        assert_eq!(p.name, "refactor-to-async");
        assert_eq!(p.triggers, vec!["refactor", "refac", "rfc"]);
        assert_eq!(p.commit_char, '>');
        assert_eq!(p.priority, 100);
        assert!(p.body.contains("Refactor this code"));
    }

    #[test]
    fn id_derived_from_filename_when_absent() {
        let raw = "---\nname: x\ntriggers: [x]\n---\nbody\n";
        let p = parse_str(raw, std::path::Path::new("/tmp/myprompt.pp.md")).unwrap();
        assert_eq!(p.id, "myprompt");
    }

    #[test]
    fn missing_frontmatter_errors() {
        let raw = "no frontmatter here";
        let err = parse_str(raw, std::path::Path::new("x")).unwrap_err();
        assert!(matches!(err, ParseError::MissingFrontmatter { .. }));
    }
}
