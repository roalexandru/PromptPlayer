//! Single source of truth for the in-memory prompt list, wrapping the lock so
//! call sites don't re-implement the same read-and-find dance.
//!
//! `generation` increments on every mutation; the picker's `SearchIndex` uses it
//! to skip rebuilds, which is what stopped the per-keystroke rebuild.

use crate::error::{AppError, AppResult};
use crate::prompts::{library, parser, Prompt, PromptOrigin};
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
    /// Library directory to write into. `None` resolves
    /// `library::default_library_root()` at each use, which is what production
    /// wants. Tests pin a temp directory: the resolver reads a process-wide
    /// env var, so parallel tests that each set it were racing each other's
    /// (already-deleted) temp directories.
    root: Option<PathBuf>,
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
            root: None,
        }
    }

    /// A store that writes into `root` regardless of the resolved library path.
    pub fn with_root(root: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Vec::new())),
            gen: Arc::new(AtomicU64::new(0)),
            root: Some(root),
        }
    }

    fn library_root(&self) -> AppResult<PathBuf> {
        self.root
            .clone()
            .or_else(library::default_library_root)
            .ok_or(AppError::LibraryRootUnresolved)
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

    /// §2.2 — "uniqueness enforced at edit time: no two prompts can share an
    /// exact trigger sequence (case-insensitive)."
    ///
    /// Without this the save succeeds, then `matcher::rebuild_index` silently
    /// drops the loser as a duplicate: one of the two prompts simply never
    /// fires again, with nothing in the UI to explain why. Checking here turns
    /// that into an error the editor can show against the trigger field.
    ///
    /// Compares against every *other* prompt (same id = the edit in progress).
    ///
    /// Only collisions that would actually take effect are rejected: the
    /// matcher indexes enabled prompts only (see `setup::rebuild_match_index`),
    /// so a disabled prompt sharing a trigger is inert. That matters for real
    /// workflows — forking a remote prompt, or keeping a disabled variant of
    /// one — which an unconditional check would make impossible. `set_enabled`
    /// re-validates, so a conflict can't be created by enabling either side.
    pub fn validate_unique_triggers(&self, prompt: &Prompt) -> AppResult<()> {
        if !prompt.enabled {
            return Ok(());
        }
        let all = self.inner.read();
        for candidate in all.iter().filter(|p| p.id != prompt.id && p.enabled) {
            for existing in &candidate.triggers {
                for incoming in &prompt.triggers {
                    // Same case-folding the matcher index uses.
                    if existing.trim().to_lowercase() == incoming.trim().to_lowercase()
                        && !incoming.trim().is_empty()
                        // A trigger only collides when the commit char matches
                        // too — `go>` and `go;` are distinct in the matcher.
                        && candidate.commit_char == prompt.commit_char
                    {
                        return Err(AppError::DuplicateTrigger {
                            trigger: incoming.clone(),
                            other_id: candidate.id.clone(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Refuse writes to prompts owned by a remote source. Their cache is
    /// replaced wholesale on refresh, so an edit would silently vanish.
    fn ensure_writable(&self, id: &str) -> AppResult<()> {
        let all = self.inner.read();
        match all.iter().find(|p| p.id == id) {
            Some(p) if p.origin.is_remote() => Err(AppError::ReadOnlyPrompt { id: id.to_string() }),
            _ => Ok(()),
        }
    }

    /// Copy a remote prompt into the local library so the user can edit it.
    /// The fork gets a fresh local id and drops the remote provenance.
    pub fn fork_to_local(&self, id: &str) -> AppResult<Prompt> {
        let source = self.require(id)?;
        if !source.origin.is_remote() {
            return Err(AppError::InvalidArg(format!(
                "prompt {id} is already local"
            )));
        }
        let root = self.library_root()?;
        std::fs::create_dir_all(&root).map_err(|e| AppError::Io {
            path: root.clone(),
            source: e,
        })?;
        // Base the local id on the file stem, not the namespaced remote id.
        let stem = id.rsplit('/').next().unwrap_or(id).to_string();
        let mut local_id = stem.clone();
        let mut path = root.join(format!("{local_id}.pp.md"));
        let mut n = 1;
        while path.exists() || self.find(&local_id).is_some() {
            n += 1;
            local_id = format!("{stem}-{n}");
            path = root.join(format!("{local_id}.pp.md"));
        }
        let forked = Prompt {
            id: local_id,
            origin: PromptOrigin::Local,
            // A fork starts disabled for the same reason the remote copy was:
            // the user should read it before it can type itself into a demo.
            enabled: false,
            source_path: Some(path),
            ..source
        };
        self.save(&forked)?;
        Ok(forked)
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
    ///
    /// Remote prompts take a different path (`commands::sources`): their
    /// enablement lives in `promptplayer.yaml`, because the cache directory is
    /// wiped on every refresh.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> AppResult<Prompt> {
        self.ensure_writable(id)?;
        if enabled {
            // Turning a prompt on is the moment its triggers start competing,
            // so this is where the check belongs — otherwise a disabled
            // duplicate could be enabled into a silent matcher collision.
            let candidate = Prompt {
                enabled: true,
                ..self.require(id)?
            };
            self.validate_unique_triggers(&candidate)?;
        }
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
        self.ensure_writable(id)?;
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

    /// Persist to the prompt's `source_path`, falling back to the stored path
    /// for that id on frontend saves. Returns the path written.
    pub fn save(&self, prompt: &Prompt) -> AppResult<PathBuf> {
        self.ensure_writable(&prompt.id)?;
        self.validate_unique_triggers(prompt)?;
        let root = self.library_root()?;
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
        // Provenance is ours to decide, not the caller's: anything written to
        // the local library root is by definition a local prompt.
        snapshot.origin = PromptOrigin::Local;
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
        self.ensure_writable(id)?;
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
        // Remove immediately: waiting for the watcher would keep a deleted
        // prompt firing, and forever if the watcher never started.
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
            newline_mode: None,
            origin: Default::default(),
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
        let path = dir.path().join("gone.pp.md");
        std::fs::write(&path, "---\nname: Gone\ntriggers: [gone]\n---\nbody").unwrap();
        let mut p = make_prompt("gone");
        p.source_path = Some(path.clone());
        let s = PromptStore::with_root(dir.path().to_path_buf());
        s.replace_all(vec![p]);
        let g = s.generation();
        s.delete("gone").unwrap();
        // Removed from memory now, not just from disk via the watcher.
        assert!(s.find("gone").is_none());
        assert!(!path.exists());
        assert!(s.generation() > g);
    }

    #[test]
    fn duplicate_triggers_are_rejected_at_edit_time() {
        // §2.2. Without this the save succeeds and `rebuild_index` silently
        // drops the loser, so one of the two prompts just never fires again.
        let s = PromptStore::new();
        s.replace_all(vec![make_prompt("first")]);
        let mut clash = make_prompt("second");
        clash.triggers = vec!["FIRST".into()]; // case-insensitive collision
        let err = s.validate_unique_triggers(&clash).unwrap_err();
        assert_eq!(err.kind(), "duplicate-trigger");
        assert!(
            err.to_string().contains("first"),
            "the message must name the other prompt: {err}"
        );
    }

    #[test]
    fn a_disabled_prompt_does_not_block_a_trigger() {
        // The matcher only indexes enabled prompts, so a disabled duplicate is
        // inert — and blocking it would make forking a remote prompt (which
        // loads disabled) impossible.
        let s = PromptStore::new();
        let mut parked = make_prompt("parked");
        parked.triggers = vec!["go".into()];
        parked.enabled = false;
        s.replace_all(vec![parked]);
        let mut active = make_prompt("active");
        active.triggers = vec!["go".into()];
        assert!(s.validate_unique_triggers(&active).is_ok());
    }

    #[test]
    fn enabling_into_an_existing_trigger_is_refused() {
        // The other half of the rule above: the check has to happen at the
        // moment a prompt starts competing for its trigger.
        let dir = tempfile::tempdir().unwrap();
        let s = PromptStore::with_root(dir.path().to_path_buf());
        // `set_enabled` writes through to disk, so both prompts need a file.
        let mut active = make_prompt("active");
        active.triggers = vec!["go".into()];
        active.source_path = Some(dir.path().join("active.pp.md"));
        let mut parked = make_prompt("parked");
        parked.triggers = vec!["go".into()];
        parked.enabled = false;
        parked.source_path = Some(dir.path().join("parked.pp.md"));
        for p in [&active, &parked] {
            std::fs::write(
                p.source_path.as_ref().unwrap(),
                parser::serialize(p).unwrap(),
            )
            .unwrap();
        }
        s.replace_all(vec![active, parked]);

        assert_eq!(
            s.set_enabled("parked", true).unwrap_err().kind(),
            "duplicate-trigger"
        );
        // Disabling is always allowed, and then the parked one can come on.
        s.set_enabled("active", false).unwrap();
        assert!(s.set_enabled("parked", true).is_ok());
    }

    #[test]
    fn editing_a_prompt_does_not_collide_with_itself() {
        let s = PromptStore::new();
        s.replace_all(vec![make_prompt("same")]);
        let edit = make_prompt("same");
        assert!(s.validate_unique_triggers(&edit).is_ok());
    }

    #[test]
    fn a_different_commit_char_is_not_a_collision() {
        // The matcher keys on (trigger, commit char), so `go>` and `go;` are
        // genuinely distinct entries.
        let s = PromptStore::new();
        s.replace_all(vec![make_prompt("go")]);
        let mut other = make_prompt("go-semi");
        other.triggers = vec!["go".into()];
        other.commit_char = ';';
        assert!(s.validate_unique_triggers(&other).is_ok());
    }

    #[test]
    fn blank_triggers_do_not_collide() {
        let s = PromptStore::new();
        let mut a = make_prompt("a");
        a.triggers = vec!["".into()];
        s.replace_all(vec![a]);
        let mut b = make_prompt("b");
        b.triggers = vec!["".into()];
        assert!(s.validate_unique_triggers(&b).is_ok());
    }

    #[test]
    fn save_refuses_a_duplicate_trigger() {
        let dir = tempfile::tempdir().unwrap();
        let s = PromptStore::with_root(dir.path().to_path_buf());
        s.replace_all(vec![make_prompt("taken")]);
        let mut clash = make_prompt("newcomer");
        clash.triggers = vec!["taken".into()];
        assert_eq!(
            s.save(&clash).unwrap_err().kind(),
            "duplicate-trigger",
            "the guard has to be wired into save, not just available"
        );
    }

    fn remote_prompt(id: &str) -> Prompt {
        Prompt {
            origin: PromptOrigin::Remote {
                source_id: "org-repo".into(),
            },
            ..make_prompt(id)
        }
    }

    #[test]
    fn remote_prompts_are_read_only() {
        let s = PromptStore::new();
        s.replace_all(vec![remote_prompt("org-repo/shared")]);
        for kind in [
            s.save(&remote_prompt("org-repo/shared"))
                .err()
                .map(|e| e.kind()),
            s.delete("org-repo/shared").err().map(|e| e.kind()),
            s.set_enabled("org-repo/shared", true)
                .err()
                .map(|e| e.kind()),
            s.set_pinned("org-repo/shared", true)
                .err()
                .map(|e| e.kind()),
        ] {
            assert_eq!(
                kind,
                Some("read-only-prompt"),
                "every write path must refuse a remote prompt"
            );
        }
    }

    #[test]
    fn save_stamps_provenance_rather_than_trusting_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        let s = PromptStore::with_root(dir.path().to_path_buf());
        // A client claiming to be remote for a brand-new prompt must not be
        // able to smuggle that provenance into the local library.
        let mut sneaky = make_prompt("local-really");
        sneaky.origin = PromptOrigin::Remote {
            source_id: "spoofed".into(),
        };
        s.save(&sneaky).unwrap();
        assert_eq!(s.find("local-really").unwrap().origin, PromptOrigin::Local);
    }

    #[test]
    fn forking_a_remote_prompt_creates_a_disabled_local_copy() {
        let dir = tempfile::tempdir().unwrap();
        let s = PromptStore::with_root(dir.path().to_path_buf());
        let mut remote = remote_prompt("org-repo/review");
        remote.name = "Review".into();
        remote.triggers = vec!["review".into()];
        // Remote prompts load disabled until reviewed, which is exactly why a
        // fork of one is allowed to keep the same trigger.
        remote.enabled = false;
        s.replace_all(vec![remote]);

        let forked = s.fork_to_local("org-repo/review").unwrap();
        assert_eq!(forked.id, "review", "namespace prefix is dropped");
        assert_eq!(forked.origin, PromptOrigin::Local);
        assert!(
            !forked.enabled,
            "a fork is unreviewed until the user says so"
        );
        assert!(forked.source_path.is_some());
        assert!(s.find("review").is_some());
    }

    #[test]
    fn forking_a_local_prompt_is_rejected() {
        let s = PromptStore::new();
        s.replace_all(vec![make_prompt("mine")]);
        assert_eq!(s.fork_to_local("mine").unwrap_err().kind(), "invalid-arg");
    }

    #[test]
    fn save_uses_existing_source_path_when_client_payload_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("custom.pp.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut existing = make_prompt("stable-id");
        existing.source_path = Some(path.clone());
        let s = PromptStore::with_root(dir.path().to_path_buf());
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
