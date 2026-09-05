//! Durable file writes. A prompt is the user's own content, and `fs::write`
//! truncates the target before it writes — a crash or a full disk mid-write
//! leaves a half file, which the library watcher then hot-reloads over the good
//! in-memory copy.

use std::io::Write;
use std::path::Path;

/// Write `contents` to `path` via a temp file in the same directory, then
/// rename over the target. `rename` is atomic within a filesystem on both APFS
/// and NTFS, so a reader sees either the old file or the new one.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;

    // Same directory, so the rename never crosses a filesystem boundary. The
    // pid keeps two processes from colliding on the same temp name.
    let stem = path.file_name().and_then(|f| f.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{stem}.{}.tmp", std::process::id()));

    let write = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents)?;
        // Flush to disk before the rename, or a crash can land an empty file
        // under the real name — worse than the truncation we're avoiding.
        f.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// [`write_atomic`] for text.
pub fn write_atomic_str(path: &Path, contents: &str) -> std::io::Result<()> {
    write_atomic(path, contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        write_atomic_str(&p, "one").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one");
        write_atomic_str(&p, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
    }

    #[test]
    fn creates_missing_parents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("deep").join("a.txt");
        write_atomic_str(&p, "x").unwrap();
        assert!(p.exists());
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        write_atomic_str(&dir.path().join("a.txt"), "x").unwrap();
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "temp file survived: {strays:?}");
    }

    #[test]
    fn temp_name_is_not_a_pp_md_the_watcher_would_load() {
        // A `*.pp.md` temp name would be picked up by the library watcher
        // mid-write, which is the failure this whole module exists to avoid.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("thing.pp.md");
        write_atomic_str(&target, "---\nname: x\n---\nbody").unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["thing.pp.md".to_string()]);
    }
}
