//! §7.4 — prompt library: load `.pp.md` files from a directory tree, watch for
//! changes, hot-reload on edit.

use crate::prompts::{parser, Prompt};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// Resolve the platform-appropriate library root (Mac: Application Support,
/// Linux/Win: config dir). Override via `PROMPT_PLAYER_PROMPTS` env var.
pub fn default_library_root() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PROMPT_PLAYER_PROMPTS") {
        return Some(PathBuf::from(p));
    }
    #[cfg(target_os = "macos")]
    {
        dirs::data_dir().map(|d| d.join("PromptPlayer").join("prompts"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::config_dir().map(|d| d.join("promptplayer").join("prompts"))
    }
}

/// Load all `.pp.md` files under `root` recursively. Skips files that fail to parse,
/// returning their errors in the second tuple element.
pub fn load_all(root: &Path) -> (Vec<Prompt>, Vec<String>) {
    let mut prompts = Vec::new();
    let mut errors = Vec::new();
    if !root.exists() {
        return (prompts, errors);
    }
    walk(root, &mut |p: &Path| {
        if !p.is_file() {
            return;
        }
        if !p
            .file_name()
            .and_then(|f| f.to_str())
            .map(|s| s.ends_with(".pp.md"))
            .unwrap_or(false)
        {
            return;
        }
        match parser::parse_file(p) {
            Ok(prompt) => prompts.push(prompt),
            Err(e) => errors.push(format!("{}: {}", p.display(), e)),
        }
    });
    (prompts, errors)
}

/// How deep the library may nest. The prompts directory is user-writable and
/// documented, so an accidental symlink loop in it would otherwise recurse
/// until the stack overflows — which, with `panic = "abort"`, kills the app at
/// startup and again on every hot-reload.
const MAX_LIBRARY_DEPTH: usize = 16;

fn walk(root: &Path, f: &mut dyn FnMut(&Path)) {
    walk_depth(root, f, 0)
}

fn walk_depth(root: &Path, f: &mut dyn FnMut(&Path), depth: usize) {
    if depth >= MAX_LIBRARY_DEPTH {
        tracing::warn!(
            "library walk hit the {} level depth cap at {:?}; not descending further",
            MAX_LIBRARY_DEPTH,
            root
        );
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        // `symlink_metadata` does not follow the link, so a directory symlink
        // is treated as a leaf rather than a branch to descend into.
        let is_symlink = std::fs::symlink_metadata(&p)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if p.is_dir() && !is_symlink {
            walk_depth(&p, f, depth + 1);
        } else if !is_symlink {
            f(&p);
        }
    }
}

/// Watcher handle. Drop to stop watching.
pub struct LibraryWatcher {
    _watcher: RecommendedWatcher,
    pub events: Receiver<LibraryEvent>,
}

#[derive(Debug)]
pub enum LibraryEvent {
    /// A `.pp.md` file was created or modified.
    Changed(PathBuf),
    /// A file was removed.
    Removed(PathBuf),
}

pub fn watch(root: &Path) -> Result<LibraryWatcher, notify::Error> {
    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        for path in event.paths {
            let is_pp = path
                .file_name()
                .and_then(|f| f.to_str())
                .map(|s| s.ends_with(".pp.md"))
                .unwrap_or(false);
            if !is_pp {
                continue;
            }
            let evt = match event.kind {
                EventKind::Remove(_) => LibraryEvent::Removed(path),
                EventKind::Create(_) | EventKind::Modify(_) => LibraryEvent::Changed(path),
                _ => continue,
            };
            let _ = tx.send(evt);
        }
    })?;
    if root.exists() {
        watcher.watch(root, RecursiveMode::Recursive)?;
    }
    Ok(LibraryWatcher {
        _watcher: watcher,
        events: rx,
    })
}

/// Coalesce a burst of file events into a single rebuild trigger (returns true
/// if at least one event occurred within the wait window).
pub fn drain_events(watcher: &LibraryWatcher, wait: Duration) -> bool {
    let mut got_any = false;
    if watcher.events.recv_timeout(wait).is_ok() {
        got_any = true;
        // Drain rest non-blocking.
        while watcher.events.try_recv().is_ok() {}
    }
    got_any
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_all_walks_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        let p = nested.join("hello.pp.md");
        let mut f = std::fs::File::create(&p).unwrap();
        write!(f, "---\nname: Hello\ntriggers: [hi]\n---\nbody").unwrap();
        let (prompts, errors) = load_all(dir.path());
        assert!(errors.is_empty(), "{:?}", errors);
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "Hello");
    }

    #[test]
    fn the_bundled_examples_all_parse() {
        // These are copied into the user's library on first run, so a broken
        // one is a broken first-run experience with no obvious cause.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("prompts-examples");
        let (prompts, errors) = load_all(&dir);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(prompts.len() >= 6, "found {}", prompts.len());
        for p in &prompts {
            assert!(!p.name.trim().is_empty(), "{} has no name", p.id);
            assert!(!p.triggers.is_empty(), "{} has no trigger", p.id);
            assert!(!p.body.trim().is_empty(), "{} has no body", p.id);
        }
    }

    #[test]
    fn the_multi_step_example_actually_splits() {
        // Guards the shipped demo of the feature, not just the parser.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("prompts-examples");
        let (prompts, _) = load_all(&dir);
        let seq = prompts
            .iter()
            .find(|p| p.id.contains("agent-followup"))
            .expect("the multi-step example is present");
        let steps = crate::prompts::steps::split_steps(&seq.body);
        assert_eq!(steps.len(), 2, "{steps:?}");
        assert!(steps[0].submit(), "the first message has to be sent");
        assert!(steps[0].wait_after.is_some());
    }

    #[test]
    #[cfg(unix)]
    fn load_all_survives_a_symlink_loop() {
        // The prompts directory is user-writable, and this used to recurse
        // until the stack overflowed — which `panic = "abort"` turns into a
        // crash at startup and on every hot-reload.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join("ok.pp.md"),
            "---\nname: Ok\ntriggers: [ok]\n---\nbody",
        )
        .unwrap();
        std::os::unix::fs::symlink(dir.path(), sub.join("loop")).unwrap();

        let (prompts, _errors) = load_all(dir.path());
        assert_eq!(prompts.len(), 1, "the real prompt is still found");
    }

    #[test]
    fn load_all_collects_errors_for_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.pp.md");
        std::fs::write(&p, "no frontmatter").unwrap();
        let (prompts, errors) = load_all(dir.path());
        assert!(prompts.is_empty());
        assert_eq!(errors.len(), 1);
    }
}
