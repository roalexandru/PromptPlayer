//! Prompt store — single source of truth for the in-memory prompt list.
//!
//! Wraps `Arc<RwLock<Vec<Prompt>>>` with cohesive methods so call sites don't
//! re-implement the same `.read()`, `.iter().find(|p| p.id == ...)` dance.
//!
//! Carries a `generation: AtomicU64` that increments on every mutation. The
//! picker's `SearchIndex` checks this to skip rebuilds when the prompts
//! haven't changed (fixes the per-keystroke rebuild regression).

use crate::error::{AppError, AppResult};
use crate::prompts::{library, parser, Prompt};
use parking_lot::{RwLock, RwLockReadGuard};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Monotonic generation counter — increments on every store mutation.
pub type StoreGeneration = u64;

#[derive(Clone)]
pub struct PromptStore {
    inner: Arc<RwLock<Vec<Prompt>>>,
    gen: Arc<AtomicU64>,
}

impl Default for PromptStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Vec::new())),
            gen: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Read-locked guard. Prefer `find`/`with_read` for short borrows.
    pub fn read(&self) -> RwLockReadGuard<'_, Vec<Prompt>> {
        self.inner.read()
    }

    /// Snapshot the entire prompt list (cloned).
    pub fn snapshot(&self) -> Vec<Prompt> {
        self.inner.read().clone()
    }

    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Generation counter — bumps on every mutation. Consumers can cache work
    /// keyed on this value (e.g., the picker's SearchIndex).
    pub fn generation(&self) -> StoreGeneration {
        self.gen.load(Ordering::Acquire)
    }

    fn bump_generation(&self) {
        self.gen.fetch_add(1, Ordering::AcqRel);
    }

    /// Replace the entire list (used on bulk load + hot reload).
    pub fn replace_all(&self, prompts: Vec<Prompt>) {
        *self.inner.write() = prompts;
        self.bump_generation();
    }

    pub fn find(&self, id: &str) -> Option<Prompt> {
        self.inner.read().iter().find(|p| p.id == id).cloned()
    }

    /// Apply a function to one prompt by id, returning the patched copy.
    pub fn modify<F>(&self, id: &str, f: F) -> AppResult<Prompt>
    where
        F: FnOnce(&mut Prompt),
    {
        let snapshot = {
            let mut all = self.inner.write();
            let p = all
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| AppError::PromptNotFound(id.to_string()))?;
            f(p);
            p.clone()
        };
        self.bump_generation();
        Ok(snapshot)
    }

    /// Toggle the per-prompt `enabled` flag and persist to disk.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> AppResult<Prompt> {
        let snapshot = self.modify(id, |p| p.enabled = enabled)?;
        let path = snapshot
            .source_path
            .clone()
            .ok_or_else(|| AppError::NoSourcePath { id: id.to_string() })?;
        let body = parser::serialize(&snapshot)?;
        std::fs::write(&path, body).map_err(|e| AppError::Io {
            path: path.clone(),
            source: e,
        })?;
        Ok(snapshot)
    }

    /// Toggle the per-prompt `pinned` flag and persist to disk. Pinned prompts
    /// surface in the tray menu (Apple Shortcuts pinned model).
    pub fn set_pinned(&self, id: &str, pinned: bool) -> AppResult<Prompt> {
        let snapshot = self.modify(id, |p| p.pinned = pinned)?;
        let path = snapshot
            .source_path
            .clone()
            .ok_or_else(|| AppError::NoSourcePath { id: id.to_string() })?;
        let body = parser::serialize(&snapshot)?;
        std::fs::write(&path, body).map_err(|e| AppError::Io {
            path: path.clone(),
            source: e,
        })?;
        Ok(snapshot)
    }

    /// Persist a prompt to its `source_path` (or, for frontend-originated
    /// saves, the existing stored source path for the same prompt id).
    /// Returns the path written to.
    pub fn save(&self, prompt: &Prompt) -> AppResult<PathBuf> {
        let root = library::default_library_root().ok_or(AppError::LibraryRootUnresolved)?;
        std::fs::create_dir_all(&root).map_err(|e| AppError::Io {
            path: root.clone(),
            source: e,
        })?;
        let existing_path = self
            .inner
            .read()
            .iter()
            .find(|p| p.id == prompt.id)
            .and_then(|p| p.source_path.clone());
        let mut snapshot = prompt.clone();
        snapshot.typing_overrides = snapshot.typing_overrides.normalized();
        if snapshot.source_path.is_none() {
            snapshot.source_path = existing_path;
        }
        let path = match &snapshot.source_path {
            Some(p) => p.clone(),
            None => root.join(format!("{}.pp.md", parser::slugify(&snapshot.id))),
        };
        let serialized = parser::serialize(&snapshot)?;
        std::fs::write(&path, serialized).map_err(|e| AppError::Io {
            path: path.clone(),
            source: e,
        })?;
        {
            let mut all = self.inner.write();
            if let Some(existing) = all.iter_mut().find(|p| p.id == snapshot.id) {
                *existing = snapshot;
            } else {
                all.push(snapshot);
            }
        }
        self.bump_generation();
        Ok(path)
    }

    /// Delete a prompt by id; removes the on-disk file.
    pub fn delete(&self, id: &str) -> AppResult<()> {
        let path = {
            let all = self.inner.read();
            let p = all
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| AppError::PromptNotFound(id.to_string()))?;
            p.source_path.clone()
        };
        if let Some(p) = path {
            std::fs::remove_file(&p).map_err(|e| AppError::Io {
                path: p.clone(),
                source: e,
            })?;
        }
        // Remove from the in-memory list immediately rather than waiting for
        // the watcher's load-all reload — otherwise a deleted prompt keeps
        // firing until the next filesystem event (and never, if the watcher
        // failed to start). The caller reindexes the matcher afterward.
        self.inner.write().retain(|p| p.id != id);
        self.bump_generation();
        Ok(())
    }

    /// Helper for IPC handlers — returns a Prompt or AppError.
    pub fn require(&self, id: &str) -> AppResult<Prompt> {
        self.find(id)
            .ok_or_else(|| AppError::PromptNotFound(id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typer::TypingOverrides;

    fn make_prompt(id: &str) -> Prompt {
        Prompt {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            triggers: vec![id.into()],
            commit_char: '>',
            priority: 0,
            typing_profile: Default::default(),
            typing_overrides: TypingOverrides::default(),
            scope: None,
            filters: Vec::new(),
            hotkey: None,
            tags: Vec::new(),
            enabled: true,
            pinned: false,
            body: "body".into(),
            source_path: None,
        }
    }

    #[test]
    fn new_store_is_empty() {
        let s = PromptStore::new();
        assert!(s.is_empty());
        assert_eq!(s.generation(), 0);
    }

    #[test]
    fn replace_all_bumps_generation() {
        let s = PromptStore::new();
        let g0 = s.generation();
        s.replace_all(vec![make_prompt("a")]);
        assert!(s.generation() > g0);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn find_returns_clone_or_none() {
        let s = PromptStore::new();
        s.replace_all(vec![make_prompt("a"), make_prompt("b")]);
        assert!(s.find("a").is_some());
        assert!(s.find("c").is_none());
    }

    #[test]
    fn modify_returns_patched_copy_and_bumps() {
        let s = PromptStore::new();
        s.replace_all(vec![make_prompt("a")]);
        let g = s.generation();
        let updated = s.modify("a", |p| p.enabled = false).unwrap();
        assert!(!updated.enabled);
        assert!(s.generation() > g);
    }

    #[test]
    fn modify_unknown_id_errors() {
        let s = PromptStore::new();
        let err = s.modify("ghost", |_| {}).unwrap_err();
        assert_eq!(err.kind(), "prompt-not-found");
    }

    #[test]
    fn require_helper_works() {
        let s = PromptStore::new();
        s.replace_all(vec![make_prompt("a")]);
        assert!(s.require("a").is_ok());
        assert_eq!(s.require("b").unwrap_err().kind(), "prompt-not-found");
    }

    #[test]
    fn snapshot_is_cloned_and_decoupled() {
        let s = PromptStore::new();
        s.replace_all(vec![make_prompt("a")]);
        let snap = s.snapshot();
        s.replace_all(Vec::new());
        // Snapshot retains old state independently.
        assert_eq!(snap.len(), 1);
        assert!(s.is_empty());
    }

    #[test]
    fn cloned_store_shares_state() {
        let s = PromptStore::new();
        let s2 = s.clone();
        s.replace_all(vec![make_prompt("x")]);
        assert_eq!(s2.len(), 1);
    }

    #[test]
    fn set_enabled_without_source_path_errors_cleanly() {
        let s = PromptStore::new();
        s.replace_all(vec![make_prompt("a")]); // source_path: None
        let err = s.set_enabled("a", false).unwrap_err();
        assert_eq!(err.kind(), "no-source-path");
        // The in-memory toggle still happened (errored on persistence).
        assert!(!s.find("a").unwrap().enabled);
    }

    #[test]
    fn delete_removes_from_memory_immediately() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("PROMPT_PLAYER_PROMPTS", dir.path());
        let path = dir.path().join("gone.pp.md");
        std::fs::write(&path, "---\nname: Gone\ntriggers: [gone]\n---\nbody").unwrap();
        let mut p = make_prompt("gone");
        p.source_path = Some(path.clone());
        let s = PromptStore::new();
        s.replace_all(vec![p]);
        let g = s.generation();
        s.delete("gone").unwrap();
        // Removed from memory now, not just from disk via the watcher.
        assert!(s.find("gone").is_none());
        assert!(!path.exists());
        assert!(s.generation() > g);
    }

    #[test]
    fn save_uses_existing_source_path_when_client_payload_has_none() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("PROMPT_PLAYER_PROMPTS", dir.path());
        let path = dir.path().join("nested").join("custom.pp.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut existing = make_prompt("stable-id");
        existing.source_path = Some(path.clone());
        let s = PromptStore::new();
        s.replace_all(vec![existing]);

        let mut payload = make_prompt("stable-id");
        payload.name = "Edited".into();
        payload.source_path = None;
        let written = s.save(&payload).unwrap();

        assert_eq!(written, path);
        assert!(path.exists());
        assert!(!dir.path().join("stable-id.pp.md").exists());
        assert_eq!(s.find("stable-id").unwrap().name, "Edited");
    }
}
