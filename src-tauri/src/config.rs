//! §7.2 / §10.3 — library-level configuration (`promptplayer.yaml`).
//!
//! The spec deliberately has no Settings *window*: cross-cutting config lives
//! in one YAML file the user edits directly, next to their prompts. This
//! module is the only reader/writer of that file.
//!
//! Everything here has a working default, so a missing or partially-broken
//! config file is never fatal: `load()` logs and falls back rather than
//! refusing to start. That matters because the file gates the global hotkeys —
//! a YAML typo must not leave the app with no way to arm or kill.
//!
//! Runtime flags that must reset every launch (armed, Keep Awake) deliberately
//! do NOT live here; see [`crate::state::AppState`].

use crate::typer::ProfileKind;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// How an embedded `\n` in a prompt body is delivered.
///
/// The default (`ShiftEnter`) is right for chat surfaces — ChatGPT, Claude,
/// Slack — where a bare Enter submits. It is *wrong* for agent TUIs running in
/// a terminal: most terminals send Shift+Enter as a plain CR, so a
/// multi-paragraph prompt submits at the first blank line. `BackslashEnter`
/// types `\` before the newline, which Claude Code and other readline-style
/// prompts interpret as a soft line break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, specta::Type)]
#[serde(rename_all = "kebab-case")]
pub enum NewlineMode {
    /// Shift+Enter — chat apps. Default.
    #[default]
    ShiftEnter,
    /// `\` then Enter — Claude Code and other terminal agent prompts.
    BackslashEnter,
    /// A bare Enter. Only safe where Enter does not submit (plain editors).
    Plain,
}

/// Which display the picker opens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, specta::Type)]
#[serde(rename_all = "kebab-case")]
pub enum PickerDisplay {
    /// Cursor's screen when mirrored, built-in screen when extended. The
    /// picker is excluded from *capture*, but a projector in extended mode is
    /// a second physical screen — nothing hides it there but placement.
    #[default]
    Auto,
    /// Always the built-in / primary display.
    Builtin,
    /// Always the screen containing the cursor (pre-Auto behavior).
    Cursor,
}

/// One remote prompt source. Only public GitHub repos are supported; the
/// fetch is anonymous (60 requests/hour) and read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "kebab-case")]
pub struct SourceSpec {
    /// `owner/repo`.
    pub repo: String,
    /// Branch, tag, or commit. Defaults to the repo's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// Only load `.pp.md` under this subdirectory of the repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    /// Skip this source without deleting the entry.
    #[serde(default = "crate::config::default_true")]
    pub enabled: bool,
}

impl SourceSpec {
    /// Stable identifier used for the on-disk cache directory and for
    /// `Prompt::origin`. Slashes and refs are folded into a flat safe name.
    pub fn id(&self) -> String {
        let mut out = String::new();
        for c in self.repo.chars() {
            out.push(if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            });
        }
        if let Some(r) = &self.git_ref {
            out.push('@');
            for c in r.chars() {
                out.push(
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                        c
                    } else {
                        '-'
                    },
                );
            }
        }
        out
    }
}

pub(crate) fn default_true() -> bool {
    true
}

fn default_commit_char() -> String {
    ">".into()
}

/// The parsed `promptplayer.yaml`. Every field is optional in the file.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "kebab-case", default)]
pub struct AppConfig {
    /// Typing profile used by prompts that don't name one.
    pub profile_default: ProfileKind,
    /// Global commit char. A prompt's own `commit-char:` still wins.
    pub commit_char_default: String,

    // ── Global hotkeys (§7.2). `None` = use the built-in default. ──
    pub hotkey_arm: Option<String>,
    pub hotkey_picker: Option<String>,
    pub hotkey_kill: Option<String>,
    pub hotkey_panic: Option<String>,
    /// Fire the next setlist cue.
    pub hotkey_next_cue: Option<String>,
    /// Pause / resume the running playback.
    pub hotkey_pause: Option<String>,
    /// Speed the running playback up / slow it down.
    pub hotkey_faster: Option<String>,
    pub hotkey_slower: Option<String>,

    /// Disarm automatically after this many minutes of being armed. 0 = never.
    /// §11 lists this as a mitigation for "fires during a private message".
    pub auto_disarm_minutes: u32,
    /// How embedded newlines are typed. Per-prompt `newline-mode:` overrides.
    pub newline_mode: NewlineMode,
    /// Refuse to type when the focused element is a password field or not
    /// editable at all (§11 password-field heuristic).
    pub text_field_guard: bool,
    /// Allow `${{ git("rev-parse --short HEAD") }}` in prompt bodies (§6.3).
    /// Off by default; read-only subcommands only; never applied to prompts
    /// from a remote source. `shell()` is not implemented — see
    /// `prompts::expressions`.
    pub allow_git_expressions: bool,
    /// Which screen the picker opens on.
    pub picker_display: PickerDisplay,
    /// Extra RDP client identifiers, merged with the built-in list (§9.3).
    pub rdp_clients: Vec<String>,
    /// Ordered prompt ids fired by the "next cue" hotkey.
    pub setlist: Vec<String>,
    /// Remote prompt sources.
    pub sources: Vec<SourceSpec>,
    /// Namespaced ids of remote prompts the user has reviewed and enabled.
    ///
    /// This lives in config rather than in the prompt file because a source's
    /// cache directory is replaced wholesale on every refresh — a flag written
    /// there would silently reset the next time the repo moved.
    pub enabled_remote: Vec<String>,
    /// Directories scanned for repo context (`$GIT_BRANCH`, `$REPO_NAME`).
    /// Usually one entry: the repo you demo from.
    pub repo_hints: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            profile_default: ProfileKind::default(),
            commit_char_default: default_commit_char(),
            hotkey_arm: None,
            hotkey_picker: None,
            hotkey_kill: None,
            hotkey_panic: None,
            hotkey_next_cue: None,
            hotkey_pause: None,
            hotkey_faster: None,
            hotkey_slower: None,
            auto_disarm_minutes: 0,
            newline_mode: NewlineMode::default(),
            text_field_guard: true,
            allow_git_expressions: false,
            picker_display: PickerDisplay::default(),
            rdp_clients: Vec::new(),
            setlist: Vec::new(),
            sources: Vec::new(),
            enabled_remote: Vec::new(),
            repo_hints: Vec::new(),
        }
    }
}

impl AppConfig {
    /// First char of `commit-char-default`, or `>` when the string is empty.
    pub fn commit_char(&self) -> char {
        self.commit_char_default.chars().next().unwrap_or('>')
    }

    /// Auto-disarm duration, or `None` when disabled.
    pub fn auto_disarm(&self) -> Option<std::time::Duration> {
        (self.auto_disarm_minutes > 0)
            .then(|| std::time::Duration::from_secs(u64::from(self.auto_disarm_minutes) * 60))
    }
}

/// The app-data root that holds `promptplayer.yaml`, `prompts/`, `sources/`
/// and `usage.json`. This is the *parent* of the prompt library root so an
/// explicit `PROMPT_PLAYER_PROMPTS` override keeps config beside the prompts.
pub fn config_root() -> Option<PathBuf> {
    if let Some(root) = crate::prompts::library::default_library_root() {
        // `…/PromptPlayer/prompts` → `…/PromptPlayer`.
        if let Some(parent) = root.parent() {
            return Some(parent.to_path_buf());
        }
        return Some(root);
    }
    None
}

pub fn config_path() -> Option<PathBuf> {
    config_root().map(|r| r.join("promptplayer.yaml"))
}

/// Cache directory for fetched remote sources.
pub fn sources_root() -> Option<PathBuf> {
    config_root().map(|r| r.join("sources"))
}

/// Read `promptplayer.yaml` from the resolved config path.
pub fn load() -> AppConfig {
    let Some(path) = config_path() else {
        return AppConfig::default();
    };
    load_at(&path)
}

/// Read a config file from an explicit path.
///
/// A missing file yields defaults silently (the common case — most users never
/// create one). A malformed file yields defaults with a warning: the app must
/// still start with working hotkeys, because this file is what defines them.
pub fn load_at(path: &std::path::Path) -> AppConfig {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return AppConfig::default(),
        Err(e) => {
            tracing::warn!("could not read {:?}: {} — using defaults", path, e);
            return AppConfig::default();
        }
    };
    match serde_yaml::from_str::<AppConfig>(&raw) {
        Ok(cfg) => {
            tracing::info!("loaded config from {:?}", path);
            cfg
        }
        Err(e) => {
            tracing::error!(
                "invalid YAML in {:?}: {} — using defaults (fix the file and restart)",
                path,
                e
            );
            AppConfig::default()
        }
    }
}

/// Write `promptplayer.yaml`. Used by the UI paths that mutate config
/// (setlist reorder, add/remove a source) so hand-edits and in-app edits
/// converge on the same file.
pub fn save(cfg: &AppConfig) -> Result<PathBuf, String> {
    let path = config_path().ok_or("could not resolve config path")?;
    save_at(&path, cfg)
}

/// Write a config file to an explicit path.
pub fn save_at(path: &std::path::Path, cfg: &AppConfig) -> Result<PathBuf, String> {
    let path = path.to_path_buf();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {parent:?}: {e}"))?;
    }
    let yaml = serde_yaml::to_string(cfg).map_err(|e| format!("serialize: {e}"))?;
    let body = format!(
        "# Prompt Player configuration (§7.2). Edit freely — the app reloads\n\
         # this file on save from the library window and on restart.\n{yaml}"
    );
    crate::fsutil::write_atomic_str(&path, &body).map_err(|e| format!("write {path:?}: {e}"))?;
    Ok(path)
}

/// Shared, mutable config handle carried on `AppContext`.
#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<RwLock<AppConfig>>,
    /// Explicit file to read and write. `None` resolves `config_path()` at
    /// each use, which is what production wants; tests pin a temp path so
    /// they never depend on (or race with) a process-wide env var.
    path: Option<PathBuf>,
}

impl Default for ConfigStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(AppConfig::default())),
            path: None,
        }
    }

    /// A store bound to one file, regardless of the resolved app-data root.
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(AppConfig::default())),
            path: Some(path),
        }
    }

    fn target_path(&self) -> Option<PathBuf> {
        self.path.clone().or_else(config_path)
    }

    /// Load from disk into the store. Called once at startup.
    pub fn load_from_disk(&self) {
        if let Some(path) = self.target_path() {
            *self.inner.write() = load_at(&path);
        }
    }

    pub fn get(&self) -> AppConfig {
        self.inner.read().clone()
    }

    pub fn set(&self, cfg: AppConfig) {
        *self.inner.write() = cfg;
    }

    /// Mutate in place and persist. Returns the written path.
    pub fn update<F>(&self, f: F) -> Result<PathBuf, String>
    where
        F: FnOnce(&mut AppConfig),
    {
        let snapshot = {
            let mut guard = self.inner.write();
            f(&mut guard);
            guard.clone()
        };
        let path = self.target_path().ok_or("could not resolve config path")?;
        save_at(&path, &snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_usable() {
        let c = AppConfig::default();
        assert_eq!(c.commit_char(), '>');
        assert_eq!(c.newline_mode, NewlineMode::ShiftEnter);
        assert!(c.text_field_guard, "guard is on by default (§11)");
        assert!(c.auto_disarm().is_none(), "0 minutes means never");
        assert!(c.sources.is_empty());
    }

    #[test]
    fn commit_char_falls_back_when_string_is_empty() {
        let c = AppConfig {
            commit_char_default: String::new(),
            ..Default::default()
        };
        assert_eq!(c.commit_char(), '>');
    }

    #[test]
    fn commit_char_takes_first_char_of_multichar_string() {
        let c = AppConfig {
            commit_char_default: ";;".into(),
            ..Default::default()
        };
        assert_eq!(c.commit_char(), ';');
    }

    #[test]
    fn auto_disarm_converts_minutes() {
        let c = AppConfig {
            auto_disarm_minutes: 15,
            ..Default::default()
        };
        assert_eq!(c.auto_disarm(), Some(std::time::Duration::from_secs(900)));
    }

    #[test]
    fn partial_yaml_keeps_defaults_for_absent_keys() {
        let yaml = "newline-mode: backslash-enter\nauto-disarm-minutes: 5\n";
        let c: AppConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.newline_mode, NewlineMode::BackslashEnter);
        assert_eq!(c.auto_disarm_minutes, 5);
        // Untouched keys still carry their defaults.
        assert_eq!(c.commit_char(), '>');
        assert!(c.text_field_guard);
    }

    #[test]
    fn source_spec_id_is_filesystem_safe() {
        let s = SourceSpec {
            repo: "roalexandru/PromptPlayer".into(),
            git_ref: Some("main".into()),
            subdir: None,
            enabled: true,
        };
        assert_eq!(s.id(), "roalexandru-PromptPlayer@main");
        assert!(!s.id().contains('/'));
    }

    #[test]
    fn source_spec_id_without_ref_omits_the_at() {
        let s = SourceSpec {
            repo: "org/team-prompts".into(),
            git_ref: None,
            subdir: None,
            enabled: true,
        };
        assert_eq!(s.id(), "org-team-prompts");
    }

    #[test]
    fn source_spec_defaults_to_enabled() {
        let s: SourceSpec = serde_yaml::from_str("repo: a/b\n").unwrap();
        assert!(s.enabled, "a listed source is active unless disabled");
    }

    #[test]
    fn round_trips_through_yaml() {
        let mut c = AppConfig::default();
        c.setlist = vec!["intro".into(), "refactor".into()];
        c.sources.push(SourceSpec {
            repo: "org/prompts".into(),
            git_ref: Some("v1".into()),
            subdir: Some("demos".into()),
            enabled: true,
        });
        c.picker_display = PickerDisplay::Builtin;
        let yaml = serde_yaml::to_string(&c).unwrap();
        let back: AppConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.setlist, c.setlist);
        assert_eq!(back.sources, c.sources);
        assert_eq!(back.picker_display, PickerDisplay::Builtin);
    }

    #[test]
    fn config_store_update_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("promptplayer.yaml");
        let store = ConfigStore::with_path(path.clone());
        let written = store
            .update(|c| c.setlist.push("cue-1".into()))
            .expect("update writes");
        assert_eq!(written, path);
        assert_eq!(store.get().setlist, vec!["cue-1".to_string()]);

        // A second store over the same file sees the change.
        let reloaded = ConfigStore::with_path(path);
        reloaded.load_from_disk();
        assert_eq!(reloaded.get().setlist, vec!["cue-1".to_string()]);
    }

    #[test]
    fn a_malformed_file_still_yields_working_defaults() {
        // The file defines the global hotkeys, so a YAML typo must not leave
        // the app with no way to arm or kill.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("promptplayer.yaml");
        std::fs::write(&path, "newline-mode: [this is not a string]\n").unwrap();
        let cfg = load_at(&path);
        assert_eq!(cfg.commit_char(), '>');
        assert_eq!(cfg.newline_mode, NewlineMode::ShiftEnter);
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_at(&dir.path().join("nope.yaml"));
        assert_eq!(cfg.newline_mode, NewlineMode::ShiftEnter);
    }

    #[test]
    fn saved_file_carries_an_explanatory_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("promptplayer.yaml");
        save_at(&path, &AppConfig::default()).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with('#'), "hand-editors get a comment first");
    }
}
