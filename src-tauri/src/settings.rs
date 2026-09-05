//! Persisted user settings — the app's first general settings store, added
//! because `armed` and Keep Awake being memory-only cost us in the field.
//!
//! Hand-rolled JSON rather than `tauri-plugin-store`: no new dependency, and
//! writes are best-effort, degrading to in-memory when the file is unwritable.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Default Keep Awake auto-off, in minutes. Outlasts a demo without allowing
/// the 3-day sessions seen in the field. `0` = indefinite, still selectable.
pub const DEFAULT_KEEP_AWAKE_MINS: u16 = 120;

/// Selectable auto-off durations, surfaced in the tray UI. `0` = indefinite.
pub const KEEP_AWAKE_CHOICES: &[u16] = &[30, 60, 120, 240, 0];

/// On-disk shape. Not exposed over IPC — the UI gets [`UiSettings`], which
/// omits bookkeeping fields it has no business seeing or editing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Opt-in: restore the previous `armed` state at launch. Defaults false,
    /// so §10.1 still holds for anyone who never opens the setting.
    pub restore_armed: bool,
    /// Last observed armed state. Only consulted when `restore_armed` is set.
    pub armed: bool,
    /// Auto-off for Keep Awake, in minutes. `0` = stay on until turned off.
    pub keep_awake_mins: u16,
    /// Opt-in: re-enable Keep Awake at launch if it was on at quit. The
    /// auto-off timer restarts, so this can't resurrect a multi-day session.
    pub restore_keep_awake: bool,
    /// Last observed Keep Awake state. Only consulted when
    /// `restore_keep_awake` is set.
    pub keep_awake: bool,
    /// An update the user dismissed; suppressed until a newer one lands.
    pub dismissed_update: Option<String>,
    /// Version already counted for `UpdateAvailableShown`, so the 6-hourly
    /// poller can't re-count the same update forever.
    pub announced_update: Option<String>,
    /// Unix seconds of the last reported `UpdateCheck { available: false }`.
    /// Throttles the second-noisiest event: 200 of 220 said "nothing new".
    pub last_no_update_report: u64,
    /// True once the user has been through the first-run setup window, so we
    /// don't reopen it on every launch.
    pub setup_seen: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            restore_armed: false,
            armed: false,
            keep_awake_mins: DEFAULT_KEEP_AWAKE_MINS,
            restore_keep_awake: false,
            keep_awake: false,
            dismissed_update: None,
            announced_update: None,
            last_no_update_report: 0,
            setup_seen: false,
        }
    }
}

/// The editable subset the UI sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct UiSettings {
    pub restore_armed: bool,
    pub keep_awake_mins: u16,
    pub restore_keep_awake: bool,
}

impl From<&Settings> for UiSettings {
    fn from(s: &Settings) -> Self {
        Self {
            restore_armed: s.restore_armed,
            keep_awake_mins: s.keep_awake_mins,
            restore_keep_awake: s.restore_keep_awake,
        }
    }
}

/// Where `settings.json` lives — sibling of `prompts/`, same app-data parent,
/// honouring the same `PROMPT_PLAYER_PROMPTS` override.
pub fn settings_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PROMPT_PLAYER_PROMPTS") {
        // The override points at the prompts dir; put settings beside it.
        return PathBuf::from(p)
            .parent()
            .map(|d| d.join("settings.json"))
            .or_else(|| Some(PathBuf::from("settings.json")));
    }
    #[cfg(target_os = "macos")]
    {
        dirs::data_dir().map(|d| d.join("PromptPlayer").join("settings.json"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::config_dir().map(|d| d.join("promptplayer").join("settings.json"))
    }
}

/// Shared, write-through settings handle. Cheap to clone.
pub struct SettingsStore {
    inner: RwLock<Settings>,
    path: Option<PathBuf>,
}

impl SettingsStore {
    /// Load from disk, falling back to defaults. A corrupt file is logged and
    /// replaced on next write — losing preferences beats refusing to start.
    pub fn load() -> Self {
        let path = settings_path();
        let inner = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| match serde_json::from_str::<Settings>(&s) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("settings.json unreadable ({}); using defaults", e);
                    None
                }
            })
            .unwrap_or_default();
        Self {
            inner: RwLock::new(inner),
            path,
        }
    }

    pub fn shared() -> Arc<Self> {
        Arc::new(Self::load())
    }

    /// In-memory only — used by tests so they never touch the real file.
    #[cfg(test)]
    pub fn ephemeral(settings: Settings) -> Self {
        Self {
            inner: RwLock::new(settings),
            path: None,
        }
    }

    pub fn get(&self) -> Settings {
        self.inner.read().clone()
    }

    /// Mutate and write through. The closure holds the write lock — keep it
    /// short and don't re-enter the store from inside it.
    pub fn update(&self, f: impl FnOnce(&mut Settings)) -> Settings {
        let snapshot = {
            let mut guard = self.inner.write();
            f(&mut guard);
            guard.clone()
        };
        self.persist(&snapshot);
        snapshot
    }

    fn persist(&self, s: &Settings) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("could not create settings dir {:?}: {}", parent, e);
                return;
            }
        }
        match serde_json::to_string_pretty(s) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    tracing::warn!("could not write {:?}: {}", path, e);
                }
            }
            Err(e) => tracing::warn!("could not serialize settings: {}", e),
        }
    }
}

impl Default for SettingsStore {
    fn default() -> Self {
        Self::load()
    }
}

/// Unix seconds, saturating to 0 rather than panicking on a bad clock.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_spec_10_1() {
        let s = Settings::default();
        assert!(
            !s.restore_armed,
            "§10.1 — an install that never opts in must still start disarmed"
        );
        assert!(!s.armed);
        assert!(!s.restore_keep_awake);
    }

    #[test]
    fn default_keep_awake_is_bounded() {
        // Out-of-box must not be the indefinite session that produced a
        // 3d14h assertion in the field.
        let s = Settings::default();
        assert_ne!(s.keep_awake_mins, 0, "default must not be indefinite");
        assert_eq!(s.keep_awake_mins, DEFAULT_KEEP_AWAKE_MINS);
        assert!(KEEP_AWAKE_CHOICES.contains(&s.keep_awake_mins));
    }

    #[test]
    fn choices_include_indefinite_exactly_once() {
        assert_eq!(
            KEEP_AWAKE_CHOICES.iter().filter(|c| **c == 0).count(),
            1,
            "0 (indefinite) must remain selectable, but only once"
        );
    }

    #[test]
    fn update_mutates_in_memory_without_a_path() {
        let store = SettingsStore::ephemeral(Settings::default());
        let out = store.update(|s| {
            s.restore_armed = true;
            s.armed = true;
        });
        assert!(out.restore_armed);
        assert!(store.get().armed);
    }

    #[test]
    fn roundtrips_through_json() {
        let mut s = Settings::default();
        s.restore_armed = true;
        s.keep_awake_mins = 30;
        s.dismissed_update = Some("0.1.9".into());
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn unknown_and_missing_fields_fall_back_to_defaults() {
        // A file written by an older or newer build must not brick startup.
        let back: Settings =
            serde_json::from_str(r#"{"restoreArmed":true,"somethingNew":42}"#).unwrap();
        assert!(back.restore_armed);
        assert_eq!(back.keep_awake_mins, DEFAULT_KEEP_AWAKE_MINS);
    }
}
