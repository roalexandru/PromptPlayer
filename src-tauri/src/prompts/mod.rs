//! §6, §7 — prompt model and library.

pub mod agent_import;
pub mod expressions;
pub mod library;
pub mod parser;
pub mod placeholders;
pub mod steps;

use crate::config::NewlineMode;
use crate::typer::{Profile, ProfileKind, TypingOverrides};
use serde::{Deserialize, Serialize};

/// Where a prompt came from, and therefore whether the app may write to it.
///
/// Not part of the `.pp.md` format — it is derived at load time (see
/// `sources::load_cached`) and carried to the frontend so the library can
/// badge remote prompts and hide their editing affordances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, specta::Type)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PromptOrigin {
    /// A file in the user's own library root. Writable.
    #[default]
    Local,
    /// Extracted from a remote source's cache. Read-only; refetching would
    /// discard any local edit, and the cache is not the user's to own.
    Remote { source_id: String },
}

impl PromptOrigin {
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    pub fn source_id(&self) -> Option<&str> {
        match self {
            Self::Remote { source_id } => Some(source_id),
            Self::Local => None,
        }
    }
}

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
    /// Per-prompt override for how embedded newlines are typed. `None` follows
    /// the library-level `newline-mode:` from `promptplayer.yaml`. Set this to
    /// `backslash-enter` for prompts aimed at a terminal agent (Claude Code),
    /// where Shift+Enter submits instead of inserting a line break.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newline_mode: Option<NewlineMode>,
    /// Where the prompt was loaded from. Derived, never stored in the file;
    /// the store re-derives it on save so a client cannot claim to be local.
    #[serde(default)]
    pub origin: PromptOrigin,
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
