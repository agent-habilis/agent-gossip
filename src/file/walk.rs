//! Filesystem walking, content hashing, and the path-traversal guard.
//!
//! [`scan`] turns a producer's file/directory into a [`Manifest`] plus the
//! canonical root path (so bodies read from the right place). [`safe_join`] and
//! [`safe_component`] validate every *received* name on the consumer before it
//! touches the filesystem — the single control that keeps a hostile producer
//! from writing outside the destination.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::RootKind;
use super::manifest::{Entry, HASH_LEN, Manifest};

/// The relative path is length-prefixed with a `u16` on the wire; refuse to
/// serve anything longer so the count can never disagree with the bytes.
const MAX_REL_PATH: usize = u16::MAX as usize;

/// Walk `root` (a file or directory), returning its kind, the name a consumer
/// should reproduce it under, its canonical path, and a hashed [`Manifest`].
/// Symlinks inside a directory are skipped (not followed, not recreated).
///
/// # Errors
/// `root` is missing, is neither a file nor a directory, has no final path
/// component (e.g. the filesystem root), contains a non-UTF-8 name, or cannot
/// be read.
pub(super) fn scan(root: &Path) -> Result<Scan> {
    let canonical =
        fs::canonicalize(root).with_context(|| format!("cannot read {}", root.display()))?;
    let name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .context("cannot serve a path with no name (e.g. the filesystem root)")?;
    let meta = fs::metadata(&canonical)?;
    if meta.is_file() {
        let hash = hash_file(&canonical)?;
        let entry = Entry {
            rel_path: name.clone(),
            size: meta.len(),
            hash,
        };
        Ok(Scan {
            kind: RootKind::File,
            name,
            canonical,
            manifest: Manifest {
                entries: vec![entry],
            },
            empty_dirs: Vec::new(),
        })
    } else if meta.is_dir() {
        let mut entries = Vec::new();
        let mut dirs = Vec::new();
        walk_dir(&canonical, &mut entries, &mut dirs)?;
        entries.sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
        let empty_dirs = empty_dirs(&entries, dirs);
        Ok(Scan {
            kind: RootKind::Dir,
            name,
            canonical,
            manifest: Manifest { entries },
            empty_dirs,
        })
    } else {
        bail!("{} is neither a file nor a directory", canonical.display());
    }
}

/// The result of walking a served path: its kind, the name a consumer recreates
/// it under, its canonical on-disk path, a hashed [`Manifest`] of its files, and
/// the relative paths of any directories that hold no files (so the consumer can
/// recreate them — file bodies alone would silently drop empty directories).
pub(super) struct Scan {
    pub(super) kind: RootKind,
    pub(super) name: String,
    pub(super) canonical: PathBuf,
    pub(super) manifest: Manifest,
    pub(super) empty_dirs: Vec<String>,
}

/// Cheaply confirm `path` can be served (exists, is a file or directory, and has
/// a final name component) without hashing anything — the fail-fast for `send`
/// before it binds an endpoint. The per-connection [`scan`] does the real work.
///
/// # Errors
/// `path` is missing, is neither a file nor a directory, or has no name.
pub(super) fn ensure_readable(path: &Path) -> Result<()> {
    let canonical =
        fs::canonicalize(path).with_context(|| format!("cannot read {}", path.display()))?;
    canonical
        .file_name()
        .and_then(|name| name.to_str())
        .context("cannot serve a path with no name (e.g. the filesystem root)")?;
    let meta = fs::metadata(&canonical)?;
    if !meta.is_file() && !meta.is_dir() {
        bail!("{} is neither a file nor a directory", canonical.display());
    }
    Ok(())
}

/// The subset of `dirs` that contain no file anywhere beneath them — the ones a
/// consumer would otherwise never learn to create. Deduped and sorted.
fn empty_dirs(entries: &[Entry], dirs: Vec<String>) -> Vec<String> {
    let mut empties: Vec<String> = dirs
        .into_iter()
        .filter(|dir| {
            let prefix = format!("{dir}/");
            !entries
                .iter()
                .any(|entry| entry.rel_path.starts_with(&prefix))
        })
        .collect();
    empties.sort();
    empties.dedup();
    empties
}

/// Build a [`Manifest`] for an existing destination directory — the consumer's
/// side of the diff. A missing directory yields an empty manifest (nothing to
/// diff against, so everything is sent).
///
/// # Errors
/// The directory exists but cannot be read, or holds a non-UTF-8 name.
pub(super) fn manifest_of_dir(root: &Path) -> Result<Manifest> {
    if !root.is_dir() {
        return Ok(Manifest::default());
    }
    let mut entries = Vec::new();
    walk_dir(root, &mut entries, &mut Vec::new())?;
    entries.sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
    Ok(Manifest { entries })
}

/// Build a single-entry [`Manifest`] for a destination file keyed by `name`, or
/// an empty one when the consumer doesn't have it yet.
///
/// # Errors
/// The file exists but cannot be read.
pub(super) fn manifest_of_file(base: &Path, name: &str) -> Result<Manifest> {
    let path = base.join(name);
    if !path.is_file() {
        return Ok(Manifest::default());
    }
    let hash = hash_file(&path)?;
    Ok(Manifest {
        entries: vec![Entry {
            rel_path: name.to_owned(),
            size: fs::metadata(&path)?.len(),
            hash,
        }],
    })
}

fn walk_dir(root: &Path, out: &mut Vec<Entry>, dirs: &mut Vec<String>) -> Result<()> {
    // Explicit stack rather than recursion so a deep tree can't blow the call
    // stack. Each frame is a directory plus its `/`-terminated relative prefix.
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let mut children = fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        // Stable order so the manifest (and the send order) is deterministic.
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
                dirs.push(rel.clone());
                stack.push((child.path(), format!("{rel}/")));
            } else if file_type.is_file() {
                let hash = hash_file(&child.path())?;
                out.push(Entry {
                    rel_path: rel,
                    size: meta.len(),
                    hash,
                });
            } else {
                tracing::debug!(path = %child.path().display(), "skipping special file");
            }
        }
    }
    Ok(())
}

/// Stream `path` through sha256 in bounded chunks — never loads the file whole.
///
/// # Errors
/// The file cannot be opened or read.
pub(super) fn hash_file(path: &Path) -> Result<[u8; HASH_LEN]> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().into())
}

/// Validate a single path component received from the peer — the building block
/// of [`safe_join`], also used to vet the root container name.
///
/// # Errors
/// Empty, `.`/`..`, NUL, or containing a path separator. `std::path::is_separator`
/// is platform-aware, so `\` is rejected on Windows (where it separates paths)
/// without over-rejecting it on unix (where it is a legal filename byte).
pub(crate) fn safe_component(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('\0')
        || name.chars().any(std::path::is_separator)
    {
        bail!("unsafe path component from peer: {name:?}");
    }
    Ok(())
}

/// Join a peer-supplied relative path onto `base`, rejecting anything that could
/// escape it (absolute paths, `..`, NUL, empty components). The returned path is
/// guaranteed to stay within `base`.
///
/// # Errors
/// Any component fails [`safe_component`], the path is empty/absolute, or the
/// joined result somehow escapes `base` (a belt-and-suspenders final check).
pub(super) fn safe_join(base: &Path, rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        bail!("empty path from peer");
    }
    if rel.starts_with('/') {
        bail!("absolute path from peer rejected: {rel:?}");
    }
    let mut path = base.to_path_buf();
    for component in rel.split('/') {
        safe_component(component)?;
        path.push(component);
    }
    // Enforced at runtime, not just in debug: even if a component slipped past
    // the checks above, a path that escaped `base` must never be written to.
    if !path.starts_with(base) {
        bail!("path from peer escapes the destination: {rel:?}");
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{safe_component, safe_join};
    use std::path::Path;

    #[test]
    fn safe_join_accepts_nested_relative_paths() {
        let joined = safe_join(Path::new("/dest"), "a/b/c.txt").expect("safe");
        assert_eq!(joined, Path::new("/dest/a/b/c.txt"));
    }

    #[test]
    fn safe_join_rejects_parent_escape() {
        assert!(safe_join(Path::new("/dest"), "../evil").is_err());
        assert!(safe_join(Path::new("/dest"), "a/../../evil").is_err());
    }

    #[test]
    fn safe_join_rejects_absolute_and_empty() {
        assert!(safe_join(Path::new("/dest"), "/etc/passwd").is_err());
        assert!(safe_join(Path::new("/dest"), "").is_err());
    }

    #[test]
    fn safe_join_rejects_nul_and_dot() {
        assert!(safe_join(Path::new("/dest"), "a\0b").is_err());
        assert!(safe_join(Path::new("/dest"), "a/./b").is_err());
    }

    #[test]
    fn safe_component_rejects_traversal_and_separators() {
        assert!(safe_component("..").is_err());
        assert!(safe_component(".").is_err());
        assert!(safe_component("a/b").is_err());
        assert!(safe_component("").is_err());
        assert!(safe_component("normal-name.txt").is_ok());
    }
}
