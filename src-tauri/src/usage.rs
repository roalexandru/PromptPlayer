//! Usage history and frecency ranking (§5.2 "recently-used prompts").
//!
//! The spec puts a recents tier at the top of the picker list; the picker
//! previously had no notion of history at all, so an empty query returned
//! prompts in filesystem order. For an agent/demo workflow the most reusable
//! prompt is almost always one fired recently, so this is what makes the
//! picker's default ordering useful.
//!
//! Storage is a small JSON file next to `promptplayer.yaml`. The spec sketched
//! an encrypted SQLite `state.db`, but nothing here is sensitive: prompt ids
//! plus counters, no bodies, no triggers, no window titles. A JSON map keeps
//! the dependency footprint at zero and stays hand-inspectable.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Half-life of a use, in days. A prompt fired once today outranks one fired
/// four times a fortnight ago, which is the behavior you want mid-demo.
const HALF_LIFE_DAYS: f64 = 7.0;

/// How many recents the picker surfaces above the full list.
pub const RECENTS_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct UsageEntry {
    /// Total fires, all time.
    pub count: u32,
    /// Unix seconds of the most recent fire.
    pub last_used: i64,
}

impl UsageEntry {
    /// Frecency: recency-decayed use count. `now` is Unix seconds.
    pub fn score(&self, now: i64) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let age_days = ((now - self.last_used).max(0) as f64) / 86_400.0;
        let decay = 0.5f64.powf(age_days / HALF_LIFE_DAYS);
        self.count as f64 * decay
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UsageFile {
    #[serde(default)]
    entries: HashMap<String, UsageEntry>,
}

/// Shared usage store. Cheap to clone (all state behind one `Arc`).
#[derive(Clone)]
pub struct UsageStore {
    inner: Arc<RwLock<HashMap<String, UsageEntry>>>,
    /// Explicit file to read and write. `None` resolves `usage_path()` at each
    /// use (production); tests pin a temp path so they neither depend on nor
    /// race with a process-wide env var.
    path: Option<PathBuf>,
}

impl Default for UsageStore {
    fn default() -> Self {
        Self::new()
    }
}

pub fn usage_path() -> Option<PathBuf> {
    crate::config::config_root().map(|r| r.join("usage.json"))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl UsageStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            path: None,
        }
    }

    /// A store bound to one file.
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            path: Some(path),
        }
    }

    fn target_path(&self) -> Option<PathBuf> {
        self.path.clone().or_else(usage_path)
    }

    /// Load from disk. A missing or corrupt file starts empty — usage history
    /// is a convenience, never worth failing a launch over.
    pub fn load_from_disk(&self) {
        let Some(path) = self.target_path() else {
            return;
        };
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!("could not read {:?}: {}", path, e);
                return;
            }
        };
        match serde_json::from_str::<UsageFile>(&raw) {
            Ok(f) => {
                let n = f.entries.len();
                *self.inner.write() = f.entries;
                tracing::info!("loaded usage history for {} prompt(s)", n);
            }
            Err(e) => tracing::warn!("ignoring malformed {:?}: {}", path, e),
        }
    }

    fn persist(&self) {
        let Some(path) = self.target_path() else {
            return;
        };
        let file = UsageFile {
            entries: self.inner.read().clone(),
        };
        let Ok(json) = serde_json::to_string_pretty(&file) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, json) {
            tracing::warn!("could not write {:?}: {}", path, e);
        }
    }

    /// Record one fire of `prompt_id` and persist. Called from the fire
    /// pipeline after a *completed* playback — a cancelled fire is not a use.
    pub fn record(&self, prompt_id: &str) {
        {
            let mut map = self.inner.write();
            let e = map.entry(prompt_id.to_string()).or_default();
            e.count = e.count.saturating_add(1);
            e.last_used = now_unix();
        }
        self.persist();
    }

    /// Forget a prompt's history (called when the prompt is deleted so the
    /// file doesn't accumulate entries for prompts that no longer exist).
    pub fn forget(&self, prompt_id: &str) {
        let removed = self.inner.write().remove(prompt_id).is_some();
        if removed {
            self.persist();
        }
    }

    pub fn entry(&self, prompt_id: &str) -> Option<UsageEntry> {
        self.inner.read().get(prompt_id).copied()
    }

    /// Prompt ids ordered by frecency, best first, capped at `limit`. Ids with
    /// no history are omitted entirely — the caller appends the remaining
    /// prompts in their own order.
    pub fn top(&self, limit: usize) -> Vec<String> {
        self.top_at(limit, now_unix())
    }

    /// `top` with an injectable clock, so the ordering is testable.
    pub fn top_at(&self, limit: usize, now: i64) -> Vec<String> {
        let map = self.inner.read();
        let mut scored: Vec<(&String, f64)> = map
            .iter()
            .filter(|(_, e)| e.count > 0)
            .map(|(id, e)| (id, e.score(now)))
            .collect();
        // Descending score; ties broken by id so the order is deterministic
        // (a HashMap iteration order would otherwise shuffle equal scores).
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(b.0))
        });
        scored
            .into_iter()
            .take(limit)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Set an entry directly. Test/import helper.
    pub fn seed(&self, prompt_id: &str, entry: UsageEntry) {
        self.inner.write().insert(prompt_id.to_string(), entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn unused_prompt_scores_zero() {
        let e = UsageEntry {
            count: 0,
            last_used: 0,
        };
        assert_eq!(e.score(DAY), 0.0);
    }

    #[test]
    fn score_decays_with_age() {
        let fresh = UsageEntry {
            count: 1,
            last_used: 100 * DAY,
        };
        let stale = UsageEntry {
            count: 1,
            last_used: 90 * DAY,
        };
        let now = 100 * DAY;
        assert!(
            fresh.score(now) > stale.score(now),
            "a more recent use must score higher for equal counts"
        );
    }

    #[test]
    fn one_half_life_halves_the_score() {
        let e = UsageEntry {
            count: 4,
            last_used: 0,
        };
        let now = (HALF_LIFE_DAYS as i64) * DAY;
        assert!((e.score(now) - 2.0).abs() < 1e-9, "got {}", e.score(now));
    }

    #[test]
    fn recent_single_use_outranks_old_burst() {
        // The behaviour the picker depends on mid-demo.
        let store = UsageStore::new();
        let now = 100 * DAY;
        store.seed(
            "old-favourite",
            UsageEntry {
                count: 4,
                last_used: now - 21 * DAY,
            },
        );
        store.seed(
            "just-used",
            UsageEntry {
                count: 1,
                last_used: now,
            },
        );
        assert_eq!(store.top_at(2, now), vec!["just-used", "old-favourite"]);
    }

    #[test]
    fn higher_count_wins_at_equal_recency() {
        let store = UsageStore::new();
        let now = 10 * DAY;
        store.seed(
            "twice",
            UsageEntry {
                count: 2,
                last_used: now,
            },
        );
        store.seed(
            "once",
            UsageEntry {
                count: 1,
                last_used: now,
            },
        );
        assert_eq!(store.top_at(2, now), vec!["twice", "once"]);
    }

    #[test]
    fn top_respects_limit_and_skips_zero_counts() {
        let store = UsageStore::new();
        let now = 0;
        for id in ["a", "b", "c"] {
            store.seed(
                id,
                UsageEntry {
                    count: 1,
                    last_used: now,
                },
            );
        }
        store.seed(
            "never",
            UsageEntry {
                count: 0,
                last_used: now,
            },
        );
        let top = store.top_at(2, now);
        assert_eq!(top.len(), 2);
        assert!(!top.contains(&"never".to_string()));
    }

    #[test]
    fn ties_are_deterministic() {
        let store = UsageStore::new();
        let now = 0;
        for id in ["zeta", "alpha", "mid"] {
            store.seed(
                id,
                UsageEntry {
                    count: 1,
                    last_used: now,
                },
            );
        }
        // Equal scores fall back to id order, not HashMap order.
        assert_eq!(store.top_at(3, now), vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn record_then_forget_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let store = UsageStore::with_path(path.clone());
        store.record("intro");
        store.record("intro");
        assert_eq!(store.entry("intro").unwrap().count, 2);

        // A fresh store reading the same file sees the same history.
        let reloaded = UsageStore::with_path(path.clone());
        reloaded.load_from_disk();
        assert_eq!(reloaded.entry("intro").unwrap().count, 2);

        reloaded.forget("intro");
        let again = UsageStore::with_path(path);
        again.load_from_disk();
        assert!(again.entry("intro").is_none(), "forget must persist too");
    }

    #[test]
    fn malformed_file_is_ignored_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        std::fs::write(&path, "{ not json").unwrap();
        let store = UsageStore::with_path(path);
        store.load_from_disk();
        assert!(
            store.top(5).is_empty(),
            "history is a convenience, never fatal"
        );
    }

    #[test]
    fn record_creates_the_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/usage.json");
        let store = UsageStore::with_path(path.clone());
        store.record("intro");
        assert!(path.exists(), "persist must mkdir -p its own directory");
    }
}
