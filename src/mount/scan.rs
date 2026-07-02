use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::wire::{DirEntry, FileEntry, MountManifest};

/// The relative path is length-prefixed with a `u16` on the wire; refuse to
/// serve anything longer so the count can never disagree with the bytes.
const MAX_REL_PATH: usize = u16::MAX as usize;

/// Walk `root` (must be a directory), returning the manifest plus one absolute
/// path per manifest file, aligned by index — the table READ requests are
/// served from. Unlike the file-transfer scan this never hashes content (the
/// tree may be huge and bytes are fetched lazily), so it is metadata-only.
/// Symlinks and special files are skipped (not followed, not listed).
///
/// # Errors
/// `root` cannot be read, or a name under it is non-UTF-8 or too long.
pub(super) fn scan(root: &Path) -> Result<(MountManifest, Vec<PathBuf>)> {
    let mut manifest = MountManifest::default();
    let mut paths = Vec::new();
    // Explicit stack rather than recursion so a deep tree can't blow the call
    // stack. Each frame is a directory plus its `/`-terminated relative prefix.
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let mut children = fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        // Stable order so the manifest (and the READ indices) is deterministic.
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            let file_name = child.file_name();
            let name = file_name
                .to_str()
                .with_context(|| format!("non-UTF-8 name under {}", dir.display()))?;
            // `symlink_metadata` does not follow the link, so a symlink is
            // detected as one rather than resolving to its target.
            let meta = fs::symlink_metadata(child.path())?;
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                tracing::debug!(path = %child.path().display(), "skipping symlink");
                continue;
            }
            let rel = format!("{prefix}{name}");
            if rel.len() > MAX_REL_PATH {
                bail!("path too long to serve: {rel}");
            }
            if file_type.is_dir() {
                manifest.dirs.push(DirEntry {
                    rel_path: rel.clone(),
                    mode: unix_mode(&meta, 0o755),
                    mtime: unix_mtime(&meta),
                });
                stack.push((child.path(), format!("{rel}/")));
            } else if file_type.is_file() {
                manifest.files.push(FileEntry {
                    rel_path: rel,
                    size: meta.len(),
                    mode: unix_mode(&meta, 0o644),
                    mtime: unix_mtime(&meta),
                });
                paths.push(child.path());
            } else {
                tracing::debug!(path = %child.path().display(), "skipping special file");
            }
        }
    }
    Ok((manifest, paths))
}

#[cfg(unix)]
fn unix_mode(meta: &fs::Metadata, _fallback: u32) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn unix_mode(_meta: &fs::Metadata, fallback: u32) -> u32 {
    fallback
}

fn unix_mtime(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|since| i64::try_from(since.as_secs()).ok())
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::scan;
    use rand::RngCore;
    use std::path::PathBuf;

    /// A throwaway directory under the OS temp dir (the repo has no `tempfile`
    /// dep); dropped recursively at the end of each test.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("ahsw-mount-test-{}", rand::rng().next_u64()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn write(path: &std::path::Path, contents: &[u8]) {
        std::fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn scan_lists_sorted_files_and_all_dirs() {
        let tmp = TempDir::new();
        let root = &tmp.path;
        std::fs::create_dir_all(root.join("b/nested")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        write(&root.join("z.txt"), b"zz");
        write(&root.join("b/nested/deep.txt"), b"deep");
        write(&root.join("a.txt"), b"a");

        let (manifest, paths) = scan(root).expect("scan");
        let dirs: Vec<&str> = manifest
            .dirs
            .iter()
            .map(|dir| dir.rel_path.as_str())
            .collect();
        assert!(dirs.contains(&"b"));
        assert!(dirs.contains(&"b/nested"));
        assert!(dirs.contains(&"empty"), "empty dirs are listed too");

        let files: Vec<(&str, u64)> = manifest
            .files
            .iter()
            .map(|file| (file.rel_path.as_str(), file.size))
            .collect();
        assert!(files.contains(&("a.txt", 1)));
        assert!(files.contains(&("z.txt", 2)));
        assert!(files.contains(&("b/nested/deep.txt", 4)));
        // paths align with manifest.files by index.
        assert_eq!(paths.len(), manifest.files.len());
        for (entry, abs) in manifest.files.iter().zip(&paths) {
            assert!(abs.ends_with(std::path::Path::new(&entry.rel_path)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn scan_skips_symlinks() {
        let tmp = TempDir::new();
        let root = &tmp.path;
        write(&root.join("real.txt"), b"real");
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();

        let (manifest, _paths) = scan(root).expect("scan");
        let names: Vec<&str> = manifest
            .files
            .iter()
            .map(|file| file.rel_path.as_str())
            .collect();
        assert_eq!(names, vec!["real.txt"]);
    }
}
