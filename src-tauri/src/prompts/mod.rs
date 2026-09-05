//! §6, §7 — prompt model and library.

pub mod expressions;
pub mod library;
pub mod parser;
pub mod placeholders;

use crate::typer::{Profile, ProfileKind, TypingOverrides};
use serde::{Deserialize, Serialize};

/// One stored prompt loaded from a `.pp.md` file.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct Prompt {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub triggers: Vec<String>,
    #[serde(default = "default_commit_char")]
    pub commit_char: char,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub typing_profile: ProfileKind,
    #[serde(default)]
    pub typing_overrides: TypingOverrides,
    #[serde(default)]
    pub scope: Option<crate::scopes::ScopeFilter>,
    #[serde(default)]
    pub filters: Vec<String>,
    #[serde(default)]
    pub hotkey: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Only enabled prompts take part in matching and hotkey registration.
    /// Defaults true so existing files keep working untouched.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// The tray shows only pinned prompts (Apple Shortcuts model); unpinned ones
    /// still fire from triggers. Defaults false so the tray stays uncrowded.
    #[serde(default)]
    pub pinned: bool,
    /// Body of the prompt — Markdown source after frontmatter.
    pub body: String,
    /// Filesystem path the prompt was loaded from. None for in-memory prompts.
    #[serde(skip)]
    pub source_path: Option<std::path::PathBuf>,
}

fn default_commit_char() -> char {
    '>'
}

fn default_enabled() -> bool {
    true
}

impl Prompt {
    /// Build the runtime profile (preset + per-prompt overrides).
    pub fn effective_profile(&self) -> Profile {
        Profile::from_kind(self.typing_profile).with_overrides(&self.typing_overrides)
    }
}
