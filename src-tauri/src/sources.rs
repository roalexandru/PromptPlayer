//! Remote prompt sources — public GitHub repositories (§7.2 `uses:` refs).
//!
//! The spec sketched `uses: github:org/team-prompts@main` as the team-sharing
//! story ("use git for now", §17). This module implements it as a read-only
//! cache: fetch the repo tarball, extract only `.pp.md` files, load them
//! alongside the local library.
//!
//! ## Why a tarball and not git
//! One anonymous request per refresh, no `git`/`libgit2` dependency, and
//! nothing to keep in sync on disk. GitHub's anonymous REST budget is 60
//! requests/hour, so a refresh first asks for the resolved commit id (a
//! ~40-byte response) and downloads the archive only when that changed.
//!
//! ## Trust posture
//! A remote prompt gets *typed into whatever the user has focused*, so it is
//! executable content, not data. Three rules follow, all enforced here:
//!
//! 1. **Read-only.** Remote prompts cannot be saved or deleted through the
//!    store; the UI offers "fork into my library" instead.
//! 2. **No hotkeys.** A `hotkey:` in a remote file is dropped on load — a
//!    third-party repo must not be able to claim a global chord.
//! 3. **Off until reviewed.** Remote prompts load disabled. Enabling one is
//!    recorded in `promptplayer.yaml` (`enabled-remote:`), which survives the
//!    cache being wiped and re-fetched.
//!
//! Ids are namespaced `<source-id>/<file stem>` so a remote prompt can never
//! shadow a local one, and the matcher's existing duplicate-trigger guard
//! resolves trigger collisions in favour of whichever was indexed first
//! (locals are indexed first — see `setup::rebuild_match_index`).

use crate::config::SourceSpec;
use crate::prompts::{library, Prompt};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// Refuse absurd archives rather than filling the user's disk. A prompt repo
/// is text; 25 MB is already three orders of magnitude more than plausible.
const MAX_ARCHIVE_BYTES: u64 = 25 * 1024 * 1024;
/// Per-file cap for an extracted `.pp.md`.
const MAX_FILE_BYTES: u64 = 512 * 1024;
/// Cap on extracted prompt files per source.
const MAX_FILES: usize = 500;
/// Optional per-repo manifest, read from the archive root.
const PACK_FILE: &str = "promptplayer-pack.yaml";
/// Where the pack manifest is cached, next to the source manifest.
const PACK_CACHE_FILE: &str = ".pp-pack.yaml";
/// GitHub requires a User-Agent on API requests.
const USER_AGENT: &str = concat!("PromptPlayer/", env!("CARGO_PKG_VERSION"));
const API: &str = "https://api.github.com";

/// A repository's own description of the prompt pack it publishes.
///
/// Entirely optional — a repo of loose `.pp.md` files works with no manifest
/// at all. Its job is to let a pack name itself, point at its own
/// subdirectory, and refuse to load into an app that is too old to understand
/// it (a pack using a feature this build lacks would otherwise fail in a
/// confusing, per-prompt way).
#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "kebab-case", default)]
pub struct PackManifest {
    pub name: Option<String>,
    pub description: Option<String>,
    /// Subdirectory holding the prompts. Used when the source entry doesn't
    /// name one of its own.
    pub subdir: Option<String>,
    /// Minimum Prompt Player version this pack needs.
    pub min_app_version: Option<String>,
}

/// Read the pack manifest out of an archive without extracting anything else.
///
/// A separate pass over the same bytes, because the manifest can redirect the
/// extraction (`subdir:`) and gate it (`min-app-version:`) — both decisions
/// have to be made before any file is written.
fn read_pack_manifest(archive: &[u8]) -> Option<PackManifest> {
    let gz = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(gz);
    for entry in tar.entries().ok()? {
        let mut entry = entry.ok()?;
        let path = entry.path().ok()?.to_path_buf();
        let Some(rel) = strip_archive_prefix(&path) else {
            continue;
        };
        if rel.to_string_lossy() != PACK_FILE {
            continue;
        }
        if entry.header().size().unwrap_or(0) > MAX_FILE_BYTES {
            return None;
        }
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut entry, &mut buf).ok()?;
        match serde_yaml::from_str::<PackManifest>(&buf) {
            Ok(m) => return Some(m),
            Err(e) => {
                tracing::warn!("ignoring malformed {}: {}", PACK_FILE, e);
                return None;
            }
        }
    }
    None
}

/// Parse `major.minor.patch` into a comparable triple, ignoring any
/// pre-release or build suffix.
///
/// Hand-rolled rather than pulling in a semver crate: this compares three
/// integers, and the app's own version is a plain `x.y.z` kept in lockstep
/// across three manifests. Returns `None` for anything that isn't three
/// numbers, which the caller treats as "no constraint".
fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    // Drop a pre-release (`-rc.1`) or build (`+meta`) suffix before splitting.
    let core = raw
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Check a pack's `min-app-version` against this build.
///
/// An unparseable requirement is ignored rather than treated as a block: a
/// typo in someone else's repo should not be able to lock a user out of their
/// own prompts.
fn check_min_version(pack: &PackManifest) -> Result<(), String> {
    let Some(raw) = pack.min_app_version.as_deref() else {
        return Ok(());
    };
    let Some(required) = parse_version(raw) else {
        tracing::warn!("ignoring unparseable min-app-version {:?}", raw);
        return Ok(());
    };
    let Some(current) = parse_version(env!("CARGO_PKG_VERSION")) else {
        return Ok(());
    };
    if current < required {
        let (a, b, c) = required;
        return Err(format!(
            "this pack needs Prompt Player {a}.{b}.{c} or newer (this is {})",
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(())
}

/// Manifest written into each source's cache directory. Records exactly what
/// was fetched so a refresh can skip an unchanged commit, and so the UI can
/// show the user which commit they are demoing from.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SourceManifest {
    pub repo: String,
    pub git_ref: Option<String>,
    pub subdir: Option<String>,
    /// Resolved commit id the cache was built from.
    pub sha: String,
    /// RFC 3339 timestamp of the fetch. A string rather than an epoch integer
    /// so the manifest stays readable by hand and the type crosses IPC without
    /// a 64-bit integer (which specta refuses to export).
    pub fetched_at: String,
    pub prompt_count: u32,
}

/// One row in the library window's Sources list.
#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub id: String,
    pub repo: String,
    pub git_ref: Option<String>,
    pub subdir: Option<String>,
    pub enabled: bool,
    /// Present once the source has been fetched at least once.
    pub manifest: Option<SourceManifest>,
    /// The repo's own `promptplayer-pack.yaml`, if it publishes one.
    pub pack: Option<PackManifest>,
    /// Web URL for the "open on GitHub" affordance.
    pub html_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// The resolved commit already matched the cache; nothing downloaded.
    UpToDate { sha: String, prompt_count: u32 },
    /// Archive downloaded and extracted.
    Updated { sha: String, prompt_count: u32 },
}

impl FetchOutcome {
    pub fn sha(&self) -> &str {
        match self {
            Self::UpToDate { sha, .. } | Self::Updated { sha, .. } => sha,
        }
    }
    pub fn prompt_count(&self) -> u32 {
        match self {
            Self::UpToDate { prompt_count, .. } | Self::Updated { prompt_count, .. } => {
                *prompt_count
            }
        }
    }
    pub fn changed(&self) -> bool {
        matches!(self, Self::Updated { .. })
    }
}

fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339()
}

/// Validate an `owner/repo` string. Rejects anything that could escape the
/// URL path or the cache directory.
pub fn parse_repo(repo: &str) -> Result<(String, String), String> {
    let trimmed = repo.trim().trim_end_matches('/');
    // Accept a pasted browser URL too — that's what a user has in hand.
    let trimmed = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("github.com/"))
        .unwrap_or(trimmed)
        .trim_end_matches(".git");
    let mut parts = trimmed.split('/');
    let owner = parts.next().unwrap_or("").trim();
    let name = parts.next().unwrap_or("").trim();
    if parts.next().is_some() {
        return Err(format!("expected `owner/repo`, got {repo:?}"));
    }
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !s.starts_with('.')
    };
    if !ok(owner) || !ok(name) {
        return Err(format!("invalid GitHub repo name {repo:?}"));
    }
    Ok((owner.to_string(), name.to_string()))
}

/// A git ref safe to interpolate into a URL path.
fn validate_ref(git_ref: &str) -> Result<(), String> {
    let bad = git_ref.is_empty()
        || git_ref.len() > 200
        || git_ref.starts_with('-')
        || git_ref.contains("..")
        || git_ref
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '?' | '#' | '%' | '\\' | ':' | '~' | '^'));
    if bad {
        return Err(format!("invalid git ref {git_ref:?}"));
    }
    Ok(())
}

pub fn cache_dir(spec: &SourceSpec) -> Option<PathBuf> {
    crate::config::sources_root().map(|r| cache_dir_in(&r, spec))
}

/// Cache directory for `spec` under an explicit sources root.
pub fn cache_dir_in(root: &Path, spec: &SourceSpec) -> PathBuf {
    root.join(spec.id())
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join(".pp-source.json")
}

pub fn read_manifest(spec: &SourceSpec) -> Option<SourceManifest> {
    let root = crate::config::sources_root()?;
    read_manifest_in(&root, spec)
}

pub fn read_manifest_in(root: &Path, spec: &SourceSpec) -> Option<SourceManifest> {
    let raw = std::fs::read_to_string(manifest_path(&cache_dir_in(root, spec))).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn status(spec: &SourceSpec) -> SourceStatus {
    let pack = crate::config::sources_root()
        .map(|root| cache_dir_in(&root, spec))
        .and_then(|dir| std::fs::read_to_string(dir.join(PACK_CACHE_FILE)).ok())
        .and_then(|raw| serde_yaml::from_str::<PackManifest>(&raw).ok());
    SourceStatus {
        id: spec.id(),
        repo: spec.repo.clone(),
        git_ref: spec.git_ref.clone(),
        subdir: spec.subdir.clone(),
        enabled: spec.enabled,
        manifest: read_manifest(spec),
        pack,
        html_url: format!("https://github.com/{}", spec.repo),
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// Resolve a ref (branch, tag, or sha) to a commit id with one small request.
///
/// `Accept: application/vnd.github.sha` makes the commits endpoint answer with
/// the bare id instead of the full commit JSON, which keeps this cheap enough
/// to run on every refresh before deciding whether to download anything.
async fn resolve_sha(
    http: &reqwest::Client,
    owner: &str,
    name: &str,
    git_ref: Option<&str>,
) -> Result<String, String> {
    let r = git_ref.unwrap_or("HEAD");
    if r != "HEAD" {
        validate_ref(r)?;
    }
    let url = format!("{API}/repos/{owner}/{name}/commits/{r}");
    let resp = http
        .get(&url)
        .header("Accept", "application/vnd.github.sha")
        .send()
        .await
        .map_err(|e| format!("resolve {r}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(describe_http_error(status, owner, name, r));
    }
    let sha = resp
        .text()
        .await
        .map_err(|e| format!("read sha: {e}"))?
        .trim()
        .to_string();
    if sha.len() < 7 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("unexpected commit id {sha:?}"));
    }
    Ok(sha)
}

/// Turn an HTTP failure into something a user can act on. The 403/404 cases
/// are the two that actually happen: rate limiting and private/typo'd repos.
fn describe_http_error(
    status: reqwest::StatusCode,
    owner: &str,
    name: &str,
    git_ref: &str,
) -> String {
    match status.as_u16() {
        404 => format!(
            "{owner}/{name}@{git_ref} not found — check the spelling, and note that \
             private repositories are not supported"
        ),
        403 | 429 => format!(
            "GitHub rate limit reached for {owner}/{name} (anonymous requests are \
             capped at 60/hour) — try again later"
        ),
        _ => format!("GitHub returned {status} for {owner}/{name}@{git_ref}"),
    }
}

/// True when `rel` is a `.pp.md` file we should extract, given an optional
/// `subdir` filter. Rejects anything that isn't a plain relative path so a
/// malicious archive can't write outside the cache directory.
pub fn wants_entry(rel: &Path, subdir: Option<&str>) -> bool {
    let is_pp = rel
        .file_name()
        .and_then(|f| f.to_str())
        .map(|s| s.ends_with(".pp.md"))
        .unwrap_or(false);
    if !is_pp {
        return false;
    }
    // Path-traversal / absolute-path guard: every component must be a plain name.
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return false;
    }
    match subdir {
        None => true,
        Some(sub) => {
            let sub = sub.trim_matches('/');
            sub.is_empty() || rel.starts_with(sub)
        }
    }
}

/// Strip the `owner-repo-sha/` wrapper GitHub puts at the root of its archives.
fn strip_archive_prefix(path: &Path) -> Option<PathBuf> {
    let mut comps = path.components();
    comps.next()?; // the wrapper directory
    let rest: PathBuf = comps.collect();
    (!rest.as_os_str().is_empty()).then_some(rest)
}

/// Extract the `.pp.md` files from a gzipped tarball into `dest`.
/// Returns the number of files written.
pub fn extract_prompts(archive: &[u8], dest: &Path, subdir: Option<&str>) -> Result<usize, String> {
    let gz = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(gz);
    let mut written = 0usize;
    for entry in tar.entries().map_err(|e| format!("read archive: {e}"))? {
        let mut entry = entry.map_err(|e| format!("archive entry: {e}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        if entry.header().size().unwrap_or(0) > MAX_FILE_BYTES {
            tracing::warn!("skipping oversized archive entry");
            continue;
        }
        let path = match entry.path() {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };
        let Some(rel) = strip_archive_prefix(&path) else {
            continue;
        };
        if !wants_entry(&rel, subdir) {
            continue;
        }
        if written >= MAX_FILES {
            tracing::warn!("source hit the {} file cap; ignoring the rest", MAX_FILES);
            break;
        }
        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create {parent:?}: {e}"))?;
        }
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf)
            .map_err(|e| format!("read entry {rel:?}: {e}"))?;
        std::fs::write(&out, &buf).map_err(|e| format!("write {out:?}: {e}"))?;
        written += 1;
    }
    Ok(written)
}

/// Read a response body, refusing it as soon as it exceeds `cap`.
async fn read_capped(mut resp: reqwest::Response, cap: u64) -> Result<Vec<u8>, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("read archive body: {e}"))?
    {
        if buf.len() as u64 + chunk.len() as u64 > cap {
            return Err(format!(
                "archive exceeded the {} MB limit mid-download",
                cap / (1024 * 1024)
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Fetch (or confirm) one source's cache.
///
/// Skips the archive download when the resolved commit already matches the
/// manifest, so a startup refresh of N sources costs N small requests.
pub async fn fetch(spec: &SourceSpec) -> Result<FetchOutcome, String> {
    let root =
        crate::config::sources_root().ok_or("could not resolve the sources cache directory")?;
    fetch_into(&root, spec).await
}

/// `fetch` against an explicit sources root.
pub async fn fetch_into(root: &Path, spec: &SourceSpec) -> Result<FetchOutcome, String> {
    let (owner, name) = parse_repo(&spec.repo)?;
    let dir = cache_dir_in(root, spec);
    let http = client()?;
    let sha = resolve_sha(&http, &owner, &name, spec.git_ref.as_deref()).await?;

    if let Some(existing) = read_manifest_in(root, spec) {
        if existing.sha == sha && dir.exists() {
            return Ok(FetchOutcome::UpToDate {
                sha,
                prompt_count: existing.prompt_count,
            });
        }
    }

    let url = format!("{API}/repos/{owner}/{name}/tarball/{sha}");
    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("download archive: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(describe_http_error(status, &owner, &name, &sha));
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_ARCHIVE_BYTES {
            return Err(format!(
                "{}/{} archive is {} MB, over the {} MB limit",
                owner,
                name,
                len / (1024 * 1024),
                MAX_ARCHIVE_BYTES / (1024 * 1024)
            ));
        }
    }
    // Stream with a running cap rather than `bytes()`. GitHub's tarball
    // endpoint is chunked and often sends no `Content-Length`, so the check
    // above can be skipped entirely — and `bytes()` buffers the whole body
    // first, meaning the post-hoc length check only fires after the memory is
    // already committed.
    let bytes = read_capped(resp, MAX_ARCHIVE_BYTES).await?;

    // Pass one: the pack manifest, which can redirect and gate what follows.
    let pack = read_pack_manifest(&bytes);
    if let Some(p) = &pack {
        check_min_version(p)?;
    }
    // An explicit `subdir:` on the source entry wins over the pack's, so a
    // user can always narrow a pack further than its author did.
    let effective_subdir = spec
        .subdir
        .clone()
        .or_else(|| pack.as_ref().and_then(|p| p.subdir.clone()));

    // Replace the cache atomically-ish: extract into a sibling, then swap.
    // A partially-extracted directory must never be loaded as a library.
    let staging = dir.with_extension("staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("create {staging:?}: {e}"))?;
    let count = match extract_prompts(&bytes, &staging, effective_subdir.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    if let Some(p) = &pack {
        if let Ok(yaml) = serde_yaml::to_string(p) {
            let _ = std::fs::write(staging.join(PACK_CACHE_FILE), yaml);
        }
    }
    let manifest = SourceManifest {
        repo: spec.repo.clone(),
        git_ref: spec.git_ref.clone(),
        subdir: spec.subdir.clone(),
        sha: sha.clone(),
        fetched_at: now_rfc3339(),
        prompt_count: count as u32,
    };
    let json = serde_json::to_string_pretty(&manifest).map_err(|e| format!("manifest: {e}"))?;
    std::fs::write(manifest_path(&staging), json).map_err(|e| format!("write manifest: {e}"))?;
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {parent:?}: {e}"))?;
    }
    std::fs::rename(&staging, &dir).map_err(|e| format!("swap cache dir: {e}"))?;

    tracing::info!(
        "source {} fetched {} prompt(s) at {}",
        spec.repo,
        count,
        &sha[..7.min(sha.len())]
    );
    Ok(FetchOutcome::Updated {
        sha,
        prompt_count: count as u32,
    })
}

/// Load every enabled source's cached prompts.
///
/// Applies the trust rules: namespaced ids, hotkeys dropped, `enabled` driven
/// by the config allow-list rather than by the remote file's own frontmatter.
pub fn load_cached(specs: &[SourceSpec], enabled_ids: &[String]) -> (Vec<Prompt>, Vec<String>) {
    let Some(root) = crate::config::sources_root() else {
        return (Vec::new(), Vec::new());
    };
    load_cached_in(&root, specs, enabled_ids)
}

/// `load_cached` against an explicit sources root.
pub fn load_cached_in(
    root: &Path,
    specs: &[SourceSpec],
    enabled_ids: &[String],
) -> (Vec<Prompt>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for spec in specs.iter().filter(|s| s.enabled) {
        let dir = cache_dir_in(root, spec);
        if !dir.exists() {
            continue;
        }
        let source_id = spec.id();
        let (prompts, errs) = library::load_all(&dir);
        for e in errs {
            errors.push(format!("[{source_id}] {e}"));
        }
        for mut p in prompts {
            // Namespace the id against the source so a remote file can never
            // shadow (or be shadowed by) a local prompt with the same stem.
            p.id = format!("{source_id}/{}", p.id);
            p.origin = crate::prompts::PromptOrigin::Remote {
                source_id: source_id.clone(),
            };
            // A third-party repo does not get to claim a global chord.
            if p.hotkey.take().is_some() {
                tracing::info!("dropped hotkey from remote prompt {}", p.id);
            }
            // Off until the user explicitly enables it in the library.
            p.enabled = enabled_ids.contains(&p.id);
            // Never surfaced in the tray until forked; pinning is a local act.
            p.pinned = false;
            out.push(p);
        }
    }
    (out, errors)
}

/// One prompt-level difference between a source's cache on disk and the
/// prompts the app currently has loaded.
#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct PendingChange {
    pub prompt_id: String,
    /// Prompt name, from whichever side has it.
    pub name: String,
    pub kind: PendingKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "kebab-case")]
pub enum PendingKind {
    Added,
    Removed,
    Changed,
}

/// Compare each source's cache against the loaded prompt set.
///
/// A startup refresh updates the caches but deliberately does not reload the
/// library — applying a third party's edits to a live, possibly-armed app
/// without saying so is the wrong default. This is what lets the UI say "3
/// prompts changed" and offer to apply, and it is computed from disk each time
/// rather than tracked as state, so it cannot drift out of sync.
pub fn pending_changes(
    specs: &[SourceSpec],
    enabled_ids: &[String],
    loaded: &[Prompt],
) -> Vec<PendingChange> {
    let Some(root) = crate::config::sources_root() else {
        return Vec::new();
    };
    pending_changes_in(&root, specs, enabled_ids, loaded)
}

/// `pending_changes` against an explicit sources root.
pub fn pending_changes_in(
    root: &Path,
    specs: &[SourceSpec],
    enabled_ids: &[String],
    loaded: &[Prompt],
) -> Vec<PendingChange> {
    let (disk, _) = load_cached_in(root, specs, enabled_ids);
    let mut out = Vec::new();
    let in_memory: Vec<&Prompt> = loaded.iter().filter(|p| p.origin.is_remote()).collect();

    for d in &disk {
        match in_memory.iter().find(|m| m.id == d.id) {
            None => out.push(PendingChange {
                prompt_id: d.id.clone(),
                name: d.name.clone(),
                kind: PendingKind::Added,
            }),
            Some(m) if m.body != d.body || m.triggers != d.triggers || m.name != d.name => out
                .push(PendingChange {
                    prompt_id: d.id.clone(),
                    name: d.name.clone(),
                    kind: PendingKind::Changed,
                }),
            Some(_) => {}
        }
    }
    for m in &in_memory {
        if !disk.iter().any(|d| d.id == m.id) {
            out.push(PendingChange {
                prompt_id: m.id.clone(),
                name: m.name.clone(),
                kind: PendingKind::Removed,
            });
        }
    }
    out.sort_by(|a, b| a.prompt_id.cmp(&b.prompt_id));
    out
}

/// Delete a source's cache directory (called when the user removes a source).
pub fn remove_cache(spec: &SourceSpec) {
    if let Some(dir) = cache_dir(spec) {
        remove_cache_dir(&dir);
    }
}

fn remove_cache_dir(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("could not remove {:?}: {}", dir, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(repo: &str) -> SourceSpec {
        SourceSpec {
            repo: repo.into(),
            git_ref: None,
            subdir: None,
            enabled: true,
        }
    }

    #[test]
    fn parses_plain_owner_repo() {
        assert_eq!(
            parse_repo("roalexandru/PromptPlayer").unwrap(),
            ("roalexandru".to_string(), "PromptPlayer".to_string())
        );
    }

    #[test]
    fn parses_pasted_browser_url() {
        // What a user actually has in their clipboard.
        for input in [
            "https://github.com/org/team-prompts",
            "github.com/org/team-prompts",
            "https://github.com/org/team-prompts.git",
            "https://github.com/org/team-prompts/",
        ] {
            assert_eq!(
                parse_repo(input).unwrap(),
                ("org".to_string(), "team-prompts".to_string()),
                "input: {input}"
            );
        }
    }

    #[test]
    fn rejects_traversal_and_junk_repos() {
        for bad in [
            "../etc/passwd",
            "org/repo/extra",
            "org",
            "",
            "org/",
            "/repo",
            "org/re po",
            "org/.hidden",
            "org/re;po",
        ] {
            assert!(parse_repo(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_unsafe_refs() {
        for bad in [
            "", "-flag", "a..b", "a b", "a#b", "a%b", "a:b", "a^b", "a~1",
        ] {
            assert!(validate_ref(bad).is_err(), "should reject ref {bad:?}");
        }
        for good in ["main", "v1.2.3", "release/2026-01", "d000125"] {
            assert!(validate_ref(good).is_ok(), "should accept ref {good:?}");
        }
    }

    #[test]
    fn wants_only_pp_md_files() {
        assert!(wants_entry(Path::new("a/b/intro.pp.md"), None));
        assert!(!wants_entry(Path::new("README.md"), None));
        assert!(!wants_entry(Path::new("script.sh"), None));
        assert!(!wants_entry(Path::new("notes.md"), None));
    }

    #[test]
    fn wants_entry_rejects_traversal_paths() {
        // The load-bearing guard: a crafted archive must not write outside
        // the cache directory.
        assert!(!wants_entry(Path::new("../evil.pp.md"), None));
        assert!(!wants_entry(Path::new("a/../../evil.pp.md"), None));
        assert!(!wants_entry(Path::new("/abs/evil.pp.md"), None));
    }

    #[test]
    fn wants_entry_honors_subdir_filter() {
        assert!(wants_entry(Path::new("demos/intro.pp.md"), Some("demos")));
        assert!(!wants_entry(Path::new("other/intro.pp.md"), Some("demos")));
        // A slash-wrapped or empty subdir means "no filter".
        assert!(wants_entry(Path::new("x/intro.pp.md"), Some("/")));
        assert!(wants_entry(Path::new("x/intro.pp.md"), Some("")));
    }

    #[test]
    fn strips_the_archive_wrapper_directory() {
        assert_eq!(
            strip_archive_prefix(Path::new("org-repo-abc123/demos/x.pp.md")),
            Some(PathBuf::from("demos/x.pp.md"))
        );
        // A bare wrapper entry has nothing left after stripping.
        assert_eq!(strip_archive_prefix(Path::new("org-repo-abc123")), None);
    }

    /// Build a gzipped tar the way GitHub does: everything under one wrapper dir.
    ///
    /// Entry names are written into the header by hand rather than through
    /// `append_data`, because the `tar` crate refuses to *write* a path
    /// containing `..` — which is exactly the archive the traversal test needs.
    fn make_tarball(files: &[(&str, &str)]) -> Vec<u8> {
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            for (name, body) in files {
                let full = format!("org-repo-abc123/{name}");
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_entry_type(tar::EntryType::Regular);
                {
                    // Raw name bytes, bypassing the crate's path validation.
                    let gnu = header.as_gnu_mut().expect("gnu header");
                    let bytes = full.as_bytes();
                    assert!(bytes.len() < gnu.name.len(), "test path too long");
                    gnu.name[..bytes.len()].copy_from_slice(bytes);
                }
                header.set_cksum();
                builder.append(&header, body.as_bytes()).unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar_buf).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn extracts_only_prompt_files() {
        let archive = make_tarball(&[
            (
                "intro.pp.md",
                "---\nname: Intro\ntriggers: [intro]\n---\nbody",
            ),
            ("README.md", "# not a prompt"),
            ("scripts/run.sh", "#!/bin/sh\nrm -rf /"),
            (
                "demos/deep.pp.md",
                "---\nname: Deep\ntriggers: [deep]\n---\nbody",
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let n = extract_prompts(&archive, dir.path(), None).unwrap();
        assert_eq!(n, 2);
        assert!(dir.path().join("intro.pp.md").exists());
        assert!(dir.path().join("demos/deep.pp.md").exists());
        assert!(!dir.path().join("README.md").exists());
        assert!(!dir.path().join("scripts/run.sh").exists());
    }

    #[test]
    fn extraction_honors_subdir() {
        let archive = make_tarball(&[
            ("top.pp.md", "---\nname: T\ntriggers: [t]\n---\nb"),
            ("demos/in.pp.md", "---\nname: I\ntriggers: [i]\n---\nb"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let n = extract_prompts(&archive, dir.path(), Some("demos")).unwrap();
        assert_eq!(n, 1);
        assert!(dir.path().join("demos/in.pp.md").exists());
        assert!(!dir.path().join("top.pp.md").exists());
    }

    #[test]
    fn extraction_ignores_traversal_entries() {
        // The load-bearing safety test: a crafted archive must not write
        // outside the cache directory.
        let archive = make_tarball(&[
            ("../../escape.pp.md", "---\nname: E\ntriggers: [e]\n---\nb"),
            ("ok.pp.md", "---\nname: O\ntriggers: [o]\n---\nb"),
        ]);
        // `dest` is nested inside its own temp dir so "outside the
        // destination" can be checked without touching the shared system
        // temp directory (which other tests and earlier runs also write to).
        let outer = tempfile::tempdir().unwrap();
        let dest = outer.path().join("cache");
        std::fs::create_dir_all(&dest).unwrap();
        let n = extract_prompts(&archive, &dest, None).unwrap();
        assert_eq!(n, 1, "only the safe entry is written");
        assert!(dest.join("ok.pp.md").exists());
        // Nothing landed outside the destination: the only child of `outer`
        // is the cache directory itself.
        let siblings: Vec<String> = std::fs::read_dir(outer.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(siblings, vec!["cache".to_string()], "escaped: {siblings:?}");
    }

    #[test]
    fn load_cached_namespaces_ids_and_applies_trust_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let s = spec("org/repo");
        let cache = cache_dir_in(root, &s);
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join("risky.pp.md"),
            "---\nname: Risky\ntriggers: [risky]\nhotkey: cmd+shift+1\nenabled: true\npinned: true\n---\nbody",
        )
        .unwrap();

        let (prompts, errors) = load_cached_in(root, &[s.clone()], &[]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(prompts.len(), 1);
        let p = &prompts[0];
        assert_eq!(p.id, format!("{}/risky", s.id()), "id must be namespaced");
        assert!(p.hotkey.is_none(), "remote hotkeys are dropped");
        assert!(!p.enabled, "remote prompts are off until reviewed");
        assert!(!p.pinned, "pinning a remote prompt is a local act");
        assert!(p.origin.is_remote());

        // Enabling is driven by the config allow-list, keyed on the namespaced id.
        let (prompts, _) = load_cached_in(root, &[s.clone()], &[format!("{}/risky", s.id())]);
        assert!(prompts[0].enabled);
    }

    #[test]
    fn load_cached_skips_disabled_sources() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut s = spec("org/off");
        let cache = cache_dir_in(root, &s);
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join("x.pp.md"),
            "---\nname: X\ntriggers: [x]\n---\nbody",
        )
        .unwrap();
        s.enabled = false;
        let (prompts, _) = load_cached_in(root, &[s], &[]);
        assert!(prompts.is_empty());
    }

    #[test]
    fn read_manifest_is_absent_before_the_first_fetch() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_manifest_in(dir.path(), &spec("org/never-fetched")).is_none());
    }

    #[test]
    fn source_status_links_to_the_repo_page() {
        let st = status(&spec("org/team-prompts"));
        assert_eq!(st.html_url, "https://github.com/org/team-prompts");
        assert!(st.enabled);
    }

    // ── pack manifest ─────────────────────────────────────────────────────

    #[test]
    fn min_version_gate_blocks_a_pack_that_needs_a_newer_build() {
        let pack = PackManifest {
            min_app_version: Some("99.0.0".into()),
            ..Default::default()
        };
        let err = check_min_version(&pack).unwrap_err();
        assert!(err.contains("99.0.0"), "{err}");
    }

    #[test]
    fn min_version_gate_allows_an_older_requirement() {
        let pack = PackManifest {
            min_app_version: Some("0.0.1".into()),
            ..Default::default()
        };
        assert!(check_min_version(&pack).is_ok());
    }

    #[test]
    fn version_parsing_handles_the_shapes_that_occur() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version(" 0.1.8 "), Some((0, 1, 8)));
        assert_eq!(parse_version("v2.0.0"), Some((2, 0, 0)));
        // The repo tags release candidates, so a pack might name one.
        assert_eq!(parse_version("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3+build7"), Some((1, 2, 3)));
        // Short forms are treated as zero-filled.
        assert_eq!(parse_version("2"), Some((2, 0, 0)));
        assert_eq!(parse_version("2.1"), Some((2, 1, 0)));
    }

    #[test]
    fn version_parsing_rejects_nonsense() {
        for bad in ["", "next", "1.x", "1.2.3.4", "a.b.c", "-1.0.0"] {
            assert!(parse_version(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn versions_compare_by_component_not_lexically() {
        // The bug a string compare would have: "0.10.0" < "0.9.0".
        assert!(parse_version("0.10.0") > parse_version("0.9.0"));
        assert!(parse_version("1.0.0") > parse_version("0.99.99"));
        assert!(parse_version("0.1.10") > parse_version("0.1.9"));
    }

    #[test]
    fn this_builds_own_version_parses() {
        // The gate silently becomes a no-op if it can't read our version.
        assert!(
            parse_version(env!("CARGO_PKG_VERSION")).is_some(),
            "CARGO_PKG_VERSION {:?} must parse",
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn an_unparseable_min_version_is_ignored_not_fatal() {
        // A typo in someone else's repo must not lock a user out of prompts.
        for raw in ["not-a-version", "", "1.x", "1.2.3.4"] {
            let pack = PackManifest {
                min_app_version: Some(raw.into()),
                ..Default::default()
            };
            assert!(check_min_version(&pack).is_ok(), "{raw:?}");
        }
    }

    #[test]
    fn no_manifest_means_no_constraint() {
        assert!(check_min_version(&PackManifest::default()).is_ok());
    }

    #[test]
    fn reads_a_pack_manifest_from_an_archive() {
        let archive = make_tarball(&[
            (
                "promptplayer-pack.yaml",
                "name: Team prompts\ndescription: Shared demo set\nsubdir: demos\n",
            ),
            ("demos/x.pp.md", "---\nname: X\ntriggers: [x]\n---\nb"),
        ]);
        let pack = read_pack_manifest(&archive).expect("manifest found");
        assert_eq!(pack.name.as_deref(), Some("Team prompts"));
        assert_eq!(pack.subdir.as_deref(), Some("demos"));
    }

    #[test]
    fn a_repo_without_a_manifest_is_fine() {
        let archive = make_tarball(&[("x.pp.md", "---\nname: X\ntriggers: [x]\n---\nb")]);
        assert!(read_pack_manifest(&archive).is_none());
    }

    #[test]
    fn a_malformed_manifest_is_ignored() {
        let archive = make_tarball(&[
            ("promptplayer-pack.yaml", "name: [this is not a string\n"),
            ("x.pp.md", "---\nname: X\ntriggers: [x]\n---\nb"),
        ]);
        assert!(read_pack_manifest(&archive).is_none());
    }

    // ── pending changes ───────────────────────────────────────────────────

    fn loaded_remote(source_id: &str, stem: &str, body: &str) -> Prompt {
        let raw = format!("---\nname: {stem}\ntriggers: [{stem}]\n---\n{body}");
        let mut p = crate::prompts::parser::parse_str(&raw, Path::new("x.pp.md")).unwrap();
        p.id = format!("{source_id}/{stem}");
        p.origin = crate::prompts::PromptOrigin::Remote {
            source_id: source_id.to_string(),
        };
        p
    }

    #[test]
    fn pending_changes_reports_added_changed_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let s = spec("org/pack");
        let sid = s.id();
        let cache = cache_dir_in(root, &s);
        std::fs::create_dir_all(&cache).unwrap();
        // On disk: `kept` unchanged, `edited` with a new body, `fresh` is new.
        std::fs::write(
            cache.join("kept.pp.md"),
            "---\nname: kept\ntriggers: [kept]\n---\nsame",
        )
        .unwrap();
        std::fs::write(
            cache.join("edited.pp.md"),
            "---\nname: edited\ntriggers: [edited]\n---\nNEW BODY",
        )
        .unwrap();
        std::fs::write(
            cache.join("fresh.pp.md"),
            "---\nname: fresh\ntriggers: [fresh]\n---\nb",
        )
        .unwrap();

        // In memory: `kept` and `edited` (old body), plus a `gone` prompt the
        // source no longer publishes.
        let loaded = vec![
            loaded_remote(&sid, "kept", "same"),
            loaded_remote(&sid, "edited", "OLD BODY"),
            loaded_remote(&sid, "gone", "b"),
            // A local prompt must never appear in a source diff.
            crate::prompts::parser::parse_str(
                "---\nname: mine\ntriggers: [mine]\n---\nlocal",
                Path::new("mine.pp.md"),
            )
            .unwrap(),
        ];

        let changes = pending_changes_in(root, &[s], &[], &loaded);
        let by_id: std::collections::HashMap<&str, PendingKind> = changes
            .iter()
            .map(|c| (c.prompt_id.as_str(), c.kind))
            .collect();
        assert_eq!(
            by_id.get(format!("{sid}/fresh").as_str()),
            Some(&PendingKind::Added)
        );
        assert_eq!(
            by_id.get(format!("{sid}/edited").as_str()),
            Some(&PendingKind::Changed)
        );
        assert_eq!(
            by_id.get(format!("{sid}/gone").as_str()),
            Some(&PendingKind::Removed)
        );
        assert!(
            !by_id.contains_key(format!("{sid}/kept").as_str()),
            "an unchanged prompt is not a pending change"
        );
        assert!(
            !by_id.contains_key("mine"),
            "local prompts are not in the diff"
        );
    }

    #[test]
    fn pending_changes_is_empty_when_the_cache_matches_memory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let s = spec("org/insync");
        let sid = s.id();
        let cache = cache_dir_in(root, &s);
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join("a.pp.md"),
            "---\nname: a\ntriggers: [a]\n---\nbody",
        )
        .unwrap();
        let loaded = vec![loaded_remote(&sid, "a", "body")];
        assert!(pending_changes_in(root, &[s], &[], &loaded).is_empty());
    }

    #[test]
    fn http_errors_explain_the_two_real_cases() {
        let rate = describe_http_error(reqwest::StatusCode::FORBIDDEN, "o", "r", "main");
        assert!(rate.contains("rate limit"), "{rate}");
        let missing = describe_http_error(reqwest::StatusCode::NOT_FOUND, "o", "r", "main");
        assert!(missing.contains("private repositories"), "{missing}");
    }
}
