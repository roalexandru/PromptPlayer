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

fn walk(root: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, f);
        } else {
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
        write!(
            f,
            "---\nname: Hello\ntriggers: [hi]\n---\nbody"
        )
        .unwrap();
        let (prompts, errors) = load_all(dir.path());
        assert!(errors.is_empty(), "{:?}", errors);
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "Hello");
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
