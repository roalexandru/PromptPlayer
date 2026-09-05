//! Repository context for agent-companion prompts.
//!
//! Coding-agent prompts routinely want to name the repo or branch they are
//! about to touch ("review the diff on $GIT_BRANCH"). The foreground app
//! tells us nothing about that, so this module derives it from the filesystem.
//!
//! Deliberately shell-free: the branch comes from reading `.git/HEAD`, not
//! from running `git`. Spawning a subprocess on every fire would be slower,
//! would flash a console window on Windows, and would make the sandboxed
//! expression helpers' "no shell by default" guarantee (§6.3) meaningless.

use std::path::{Path, PathBuf};

/// What we could work out about the repo the user is demoing from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoContext {
    /// Repository directory name, e.g. `PromptPlayer`.
    pub name: Option<String>,
    /// Current branch, or a short commit id when HEAD is detached.
    pub branch: Option<String>,
    /// Absolute path of the repository root.
    pub root: Option<String>,
}

impl RepoContext {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.branch.is_none() && self.root.is_none()
    }
}

/// Walk up from `start` looking for a `.git` entry. Returns the directory that
/// contains it. Bounded to 40 levels so a pathological path can't spin.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    for _ in 0..40 {
        let dir = cur?;
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// Read the current branch from a repo root.
///
/// `.git/HEAD` is either `ref: refs/heads/<branch>` on a branch, or a raw
/// 40-char commit id when detached. A worktree or submodule has a `.git`
/// *file* pointing elsewhere (`gitdir: …`); follow that one hop.
pub fn read_branch(repo_root: &Path) -> Option<String> {
    let dot_git = repo_root.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // `gitdir: /abs/path/to/.git/worktrees/foo`
        let raw = std::fs::read_to_string(&dot_git).ok()?;
        let rest = raw.trim().strip_prefix("gitdir:")?.trim();
        let p = PathBuf::from(rest);
        if p.is_absolute() {
            p
        } else {
            repo_root.join(p)
        }
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(refname) = head.strip_prefix("ref:") {
        let refname = refname.trim();
        return Some(
            refname
                .strip_prefix("refs/heads/")
                .unwrap_or(refname)
                .to_string(),
        );
    }
    // Detached HEAD — a bare commit id. Short form is what a prompt wants.
    if head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(head[..7].to_string());
    }
    None
}

/// Build a context for the repo containing `path`.
pub fn context_for(path: &Path) -> RepoContext {
    let Some(root) = find_repo_root(path) else {
        return RepoContext::default();
    };
    RepoContext {
        name: root
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string()),
        branch: read_branch(&root),
        root: Some(root.to_string_lossy().into_owned()),
    }
}

/// True when `s` starts with something that can only be an absolute path:
/// a POSIX root, a Windows drive letter, or a UNC prefix.
fn absolute_path_start(s: &str) -> bool {
    let b = s.as_bytes();
    match b.first() {
        Some(b'/') => s.len() > 1,
        Some(b'\\') => s.starts_with("\\\\"), // UNC \\server\share
        Some(c) if c.is_ascii_alphabetic() => {
            // `C:\…` or `C:/…`
            matches!(b.get(1), Some(b':')) && matches!(b.get(2), Some(b'\\') | Some(b'/'))
        }
        _ => false,
    }
}

/// Pull filesystem-path-looking candidates out of a window title.
///
/// Terminal emulators put the working directory in the title, but the format
/// varies wildly (`~/src/app`, `app — user@host`, `nvim ~/src/app/x.rs`,
/// `C:\src\app`), so this is deliberately generous and the caller verifies
/// which candidates exist.
///
/// Two wrinkles the obvious "split on whitespace" version gets wrong:
///
/// - **Windows paths.** A drive letter or UNC prefix has to count as a root,
///   or title detection silently never fires on Windows — which is exactly
///   what the CI run for this change caught.
/// - **Spaces in paths.** `C:\Program Files\app` and `/Users/me/My Project`
///   are ordinary. So from each start marker we emit the whole remainder of
///   the title first, then progressively shorter prefixes (dropping one
///   trailing word at a time). The caller takes the first that exists, which
///   is the longest real path — and a title like `nvim /src/app — idle` still
///   resolves, because `/src/app` is reached on a later shrink.
pub fn paths_in_title(title: &str, home: Option<&Path>) -> Vec<PathBuf> {
    /// Bound on candidates per marker, so a pathological title stays cheap.
    const MAX_SHRINKS: usize = 12;
    let trim = |s: &str| {
        s.trim()
            .trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | ',' | ';' | '"' | '\''))
            .to_string()
    };
    let mut out = Vec::new();
    // Word start offsets, so each marker is examined once.
    let starts = std::iter::once(0).chain(
        title
            .char_indices()
            .filter(|(_, c)| c.is_whitespace() || *c == '|')
            .map(|(i, c)| i + c.len_utf8()),
    );
    for start in starts {
        let rest = &title[start..];
        let rest_trimmed = rest.trim_start_matches(|c: char| matches!(c, '(' | '[' | '"' | '\''));
        let (prefix, expanded_home): (&str, Option<PathBuf>) =
            if let Some(after) = rest_trimmed.strip_prefix("~/") {
                match home {
                    Some(h) => (after, Some(h.to_path_buf())),
                    None => continue,
                }
            } else if rest_trimmed == "~" || rest_trimmed.starts_with("~ ") {
                if let Some(h) = home {
                    out.push(h.to_path_buf());
                }
                continue;
            } else if absolute_path_start(rest_trimmed) {
                (rest_trimmed, None)
            } else {
                continue;
            };

        // Cut at the first closing delimiter: a title that wraps the path in
        // brackets or quotes (`agent (/srv/checkout) idle`) would otherwise
        // carry the bracket and everything after it into the candidate.
        let prefix = match prefix.find(|c: char| matches!(c, ')' | ']' | '"' | '\'' | '|')) {
            Some(i) => &prefix[..i],
            None => prefix,
        };
        // Longest first, then drop one trailing word at a time.
        let mut candidate = trim(prefix);
        for _ in 0..MAX_SHRINKS {
            if candidate.is_empty() {
                break;
            }
            let path = match &expanded_home {
                Some(h) => h.join(&candidate),
                None => PathBuf::from(&candidate),
            };
            if !out.contains(&path) {
                out.push(path);
            }
            match candidate.rfind(|c: char| c.is_whitespace() || c == '|') {
                Some(i) => candidate = trim(&candidate[..i]),
                None => break,
            }
        }
    }
    out
}

/// Resolve repo context for a fire.
///
/// Order of preference:
/// 1. A path found in the foreground window title (what the user is *looking*
///    at — a terminal running an agent in some checkout).
/// 2. Each `repo-hints:` entry from the config, in order.
///
/// Non-existent candidates are skipped, so a stale hint or a title that merely
/// looked path-shaped costs nothing.
pub fn resolve(window_title: Option<&str>, hints: &[String]) -> RepoContext {
    let home = dirs::home_dir();
    if let Some(title) = window_title {
        for cand in paths_in_title(title, home.as_deref()) {
            // A title may name a file (`nvim ~/src/app/main.rs`); its parent
            // is the directory we care about.
            let dir = if cand.is_dir() {
                Some(cand)
            } else if cand.is_file() {
                cand.parent().map(|p| p.to_path_buf())
            } else {
                None
            };
            if let Some(dir) = dir {
                let ctx = context_for(&dir);
                if !ctx.is_empty() {
                    return ctx;
                }
            }
        }
    }
    for hint in hints {
        let expanded = match (hint.strip_prefix("~/"), home.as_deref()) {
            (Some(rest), Some(h)) => h.join(rest),
            _ => PathBuf::from(hint),
        };
        if expanded.is_dir() {
            let ctx = context_for(&expanded);
            if !ctx.is_empty() {
                return ctx;
            }
        }
    }
    RepoContext::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake repo: `<tmp>/<name>/.git/HEAD` containing `head`.
    fn fake_repo(dir: &Path, name: &str, head: &str) -> PathBuf {
        let root = dir.join(name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git").join("HEAD"), head).unwrap();
        root
    }

    #[test]
    fn reads_branch_from_symbolic_head() {
        let dir = tempfile::tempdir().unwrap();
        let root = fake_repo(dir.path(), "myrepo", "ref: refs/heads/feature/x\n");
        assert_eq!(read_branch(&root).as_deref(), Some("feature/x"));
    }

    #[test]
    fn detached_head_reports_short_sha() {
        let dir = tempfile::tempdir().unwrap();
        let root = fake_repo(
            dir.path(),
            "myrepo",
            "d000125abcdef0123456789abcdef0123456789a\n",
        );
        assert_eq!(read_branch(&root).as_deref(), Some("d000125"));
    }

    #[test]
    fn garbage_head_yields_no_branch() {
        let dir = tempfile::tempdir().unwrap();
        let root = fake_repo(dir.path(), "myrepo", "not a ref\n");
        assert!(read_branch(&root).is_none());
    }

    #[test]
    fn worktree_git_file_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        // Real repo with the actual git dir…
        let real = fake_repo(dir.path(), "real", "ref: refs/heads/main\n");
        // …and a worktree whose `.git` is a FILE pointing at it.
        let wt = dir.path().join("worktree");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", real.join(".git").display()),
        )
        .unwrap();
        assert_eq!(read_branch(&wt).as_deref(), Some("main"));
    }

    #[test]
    fn finds_root_from_nested_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = fake_repo(dir.path(), "myrepo", "ref: refs/heads/main\n");
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_repo_root(&nested), Some(root));
    }

    #[test]
    fn no_git_anywhere_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        // A temp dir is not inside a repo checkout.
        assert!(find_repo_root(&plain).is_none());
    }

    #[test]
    fn context_for_populates_name_and_branch() {
        let dir = tempfile::tempdir().unwrap();
        let root = fake_repo(dir.path(), "PromptPlayer", "ref: refs/heads/main\n");
        let ctx = context_for(&root);
        assert_eq!(ctx.name.as_deref(), Some("PromptPlayer"));
        assert_eq!(ctx.branch.as_deref(), Some("main"));
        assert!(!ctx.is_empty());
    }

    #[test]
    fn extracts_absolute_and_tilde_paths_from_titles() {
        let home = PathBuf::from("/Users/demo");
        let got = paths_in_title("nvim ~/src/app/main.rs — /tmp/scratch", Some(&home));
        assert!(got.contains(&PathBuf::from("/Users/demo/src/app/main.rs")));
        assert!(got.contains(&PathBuf::from("/tmp/scratch")));
    }

    #[test]
    fn ignores_non_path_title_words() {
        let got = paths_in_title("Claude Code — main branch", None);
        assert!(got.is_empty(), "got {got:?}");
    }

    #[test]
    fn strips_punctuation_and_trailing_words_around_path_tokens() {
        let got = paths_in_title("agent (/srv/checkout) idle", None);
        assert_eq!(got, vec![PathBuf::from("/srv/checkout")]);
    }

    #[test]
    fn recognises_windows_paths() {
        // Without a drive-letter root, title detection silently never fired on
        // Windows — which is what the CI run for this change caught.
        let got = paths_in_title(r"agent C:\src\app", None);
        assert!(got.contains(&PathBuf::from(r"C:\src\app")), "{got:?}");

        let unc = paths_in_title(r"\\build\share\repo", None);
        assert!(
            unc.contains(&PathBuf::from(r"\\build\share\repo")),
            "{unc:?}"
        );
    }

    #[test]
    fn offers_candidates_longest_first_so_paths_with_spaces_resolve() {
        // `C:\Program Files\app` and `/Users/me/My Project` are ordinary, so
        // the whitespace-split-only version could never match them.
        let got = paths_in_title("/Users/me/My Project — idle", None);
        assert_eq!(
            got.first(),
            Some(&PathBuf::from("/Users/me/My Project — idle")),
            "longest candidate first: {got:?}"
        );
        assert!(
            got.contains(&PathBuf::from("/Users/me/My Project")),
            "and the real path is reachable by shrinking: {got:?}"
        );
    }

    #[test]
    fn relative_paths_are_not_candidates() {
        // A bare word or a relative path can't be resolved and would only
        // produce false positives.
        for title in ["src/app", "app — idle", r"src\app", "C:", "just words"] {
            let got = paths_in_title(title, None);
            assert!(got.is_empty(), "{title:?} produced {got:?}");
        }
    }

    #[test]
    fn resolve_picks_the_longest_existing_path_from_a_title_with_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let root = fake_repo(dir.path(), "My Project", "ref: refs/heads/spaced\n");
        let title = format!("nvim {} — idle", root.display());
        let ctx = resolve(Some(&title), &[]);
        assert_eq!(ctx.name.as_deref(), Some("My Project"));
        assert_eq!(ctx.branch.as_deref(), Some("spaced"));
    }

    #[test]
    fn resolve_prefers_window_title_over_hints() {
        let dir = tempfile::tempdir().unwrap();
        let titled = fake_repo(dir.path(), "from-title", "ref: refs/heads/title-branch\n");
        let hinted = fake_repo(dir.path(), "from-hint", "ref: refs/heads/hint-branch\n");
        let title = format!("agent {}", titled.display());
        let ctx = resolve(Some(&title), &[hinted.to_string_lossy().into_owned()]);
        assert_eq!(ctx.name.as_deref(), Some("from-title"));
        assert_eq!(ctx.branch.as_deref(), Some("title-branch"));
    }

    #[test]
    fn resolve_falls_back_to_hints_when_title_has_no_repo() {
        let dir = tempfile::tempdir().unwrap();
        let hinted = fake_repo(dir.path(), "from-hint", "ref: refs/heads/hint-branch\n");
        let ctx = resolve(
            Some("Claude Code — no paths here"),
            &[hinted.to_string_lossy().into_owned()],
        );
        assert_eq!(ctx.branch.as_deref(), Some("hint-branch"));
    }

    #[test]
    fn resolve_skips_nonexistent_hints() {
        let ctx = resolve(None, &["/definitely/not/here".into()]);
        assert!(ctx.is_empty());
    }

    #[test]
    fn resolve_uses_parent_when_title_names_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = fake_repo(dir.path(), "repo", "ref: refs/heads/main\n");
        let file = root.join("README.md");
        std::fs::write(&file, "hi").unwrap();
        let title = format!("vim {}", file.display());
        let ctx = resolve(Some(&title), &[]);
        assert_eq!(ctx.name.as_deref(), Some("repo"));
    }
}
