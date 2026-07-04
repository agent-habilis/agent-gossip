//! The per-peer blob store: a content-addressed map of **spool-file paths**
//! (never the bytes — serving streams from disk, so memory stays constant
//! regardless of blob size). On offload a file is snapshotted into the spool by
//! hardlink (same filesystem, ≈free) or copy (cross-fs fallback), so the
//! producing agent deleting or overwriting the original can't disturb the served
//! bytes. Spool files are named by hex SHA-256 — traversal-safe and idempotent.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::a2a::TaskId;
use crate::util::consts::MAX_BLOB_STORE_BYTES;

use super::{HASH_LEN, SECRET_LEN};

/// One spooled blob: the metadata the fetch path needs, plus the spool path to
/// stream from. No bytes are held.
pub(crate) struct BlobEntry {
    pub path: PathBuf,
    pub size: u64,
    pub secret: [u8; SECRET_LEN],
    task_id: TaskId,
    /// Monotonic insertion order, for oldest-first eviction under disk pressure.
    seq: u64,
}

/// One store per peer (per daemon). Holds the blobs this peer offloaded — its
/// outbound inputs and produced outputs — content-addressed, never replicated.
pub(crate) struct BlobStore {
    spool_dir: PathBuf,
    map: HashMap<[u8; HASH_LEN], BlobEntry>,
    total_bytes: u64,
    next_seq: u64,
}

impl BlobStore {
    /// Create the store, ensuring its spool directory exists.
    ///
    /// # Errors
    /// The spool directory cannot be created.
    pub(crate) fn new(spool_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&spool_dir).with_context(|| {
            format!("creating the blob spool dir {} failed", spool_dir.display())
        })?;
        Ok(Self {
            spool_dir,
            map: HashMap::new(),
            total_bytes: 0,
            next_seq: 0,
        })
    }

    /// Snapshot `src` into the spool under `sha256`, owned by `task_id`. Returns
    /// the bearer secret the ticket must carry: the existing one if this content
    /// is already spooled (content-addressed dedup — first registration wins),
    /// else `proposed`.
    ///
    /// # Errors
    /// Snapshotting the file into the spool fails.
    pub(crate) fn snapshot(
        &mut self,
        src: &Path,
        sha256: [u8; HASH_LEN],
        size: u64,
        proposed: [u8; SECRET_LEN],
        task_id: TaskId,
    ) -> Result<[u8; SECRET_LEN]> {
        if let Some(entry) = self.map.get(&sha256) {
            return Ok(entry.secret);
        }
        let dest = self.spool_dir.join(hex_encode(&sha256));
        snapshot_file(src, &dest)?;
        let seq = self.next_seq;
        self.next_seq += 1;
        self.total_bytes += size;
        self.map.insert(
            sha256,
            BlobEntry {
                path: dest,
                size,
                secret: proposed,
                task_id,
                seq,
            },
        );
        self.evict_to_budget();
        Ok(proposed)
    }

    /// The entry for `hash`, if spooled.
    pub(crate) fn get(&self, hash: &[u8; HASH_LEN]) -> Option<&BlobEntry> {
        self.map.get(hash)
    }

    /// Drop every blob owned by `task_id` (called from the task idle-timeout
    /// sweep), unlinking its spool file.
    pub(crate) fn evict_task(&mut self, task_id: &TaskId) {
        let doomed: Vec<[u8; HASH_LEN]> = self
            .map
            .iter()
            .filter(|(_, entry)| &entry.task_id == task_id)
            .map(|(hash, _)| *hash)
            .collect();
        for hash in doomed {
            self.remove(&hash);
        }
    }

    /// Unlink oldest-first until the store is within `MAX_BLOB_STORE_BYTES`.
    fn evict_to_budget(&mut self) {
        while self.total_bytes > MAX_BLOB_STORE_BYTES {
            let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, entry)| entry.seq)
                .map(|(hash, _)| *hash)
            else {
                break;
            };
            self.remove(&oldest);
        }
    }

    fn remove(&mut self, hash: &[u8; HASH_LEN]) {
        if let Some(entry) = self.map.remove(hash) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.size);
            let _ = fs::remove_file(&entry.path);
        }
    }
}

impl Drop for BlobStore {
    fn drop(&mut self) {
        // Best-effort: on daemon shutdown/leave the whole spool dir goes.
        let _ = fs::remove_dir_all(&self.spool_dir);
    }
}

/// Hardlink `src` into the spool at `dest`, falling back to a copy when the two
/// are on different filesystems (hardlinks can't cross a mount). Idempotent — a
/// pre-existing `dest` (same content, since it's hash-named) is left as is.
fn snapshot_file(src: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    if fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    fs::copy(src, dest)
        .with_context(|| format!("snapshotting {} into the blob spool failed", src.display()))?;
    Ok(())
}

/// Lowercase hex of `bytes` — the spool filename for a content hash.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::BlobStore;
    use crate::a2a::TaskId;
    use crate::blob::{HASH_LEN, SECRET_LEN};
    use rand::RngCore;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ahsw-blob-{tag}-{}", rand::rng().next_u64()));
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn snapshot_then_get_streams_from_spool() {
        let src_dir = temp_dir("src");
        let src = src_dir.join("payload.bin");
        fs::write(&src, vec![7u8; 4096]).unwrap();

        let mut store = BlobStore::new(temp_dir("spool")).unwrap();
        let hash = [1u8; HASH_LEN];
        let secret = store
            .snapshot(&src, hash, 4096, [9u8; SECRET_LEN], TaskId::random())
            .unwrap();
        assert_eq!(secret, [9u8; SECRET_LEN]);

        let entry = store.get(&hash).expect("entry present");
        assert_eq!(entry.size, 4096);
        assert_eq!(fs::read(&entry.path).unwrap(), vec![7u8; 4096]);

        fs::remove_dir_all(&src_dir).ok();
    }

    #[test]
    fn dedup_keeps_the_first_secret() {
        let src_dir = temp_dir("src");
        let src = src_dir.join("payload.bin");
        fs::write(&src, b"same content").unwrap();

        let mut store = BlobStore::new(temp_dir("spool")).unwrap();
        let hash = [2u8; HASH_LEN];
        let first = store
            .snapshot(&src, hash, 12, [1u8; SECRET_LEN], TaskId::random())
            .unwrap();
        let second = store
            .snapshot(&src, hash, 12, [2u8; SECRET_LEN], TaskId::random())
            .unwrap();
        assert_eq!(first, [1u8; SECRET_LEN]);
        assert_eq!(
            second, first,
            "dedup must return the first registration's secret"
        );

        fs::remove_dir_all(&src_dir).ok();
    }

    #[test]
    fn evict_task_unlinks_the_spool_file() {
        let src_dir = temp_dir("src");
        let src = src_dir.join("payload.bin");
        fs::write(&src, b"bye").unwrap();

        let mut store = BlobStore::new(temp_dir("spool")).unwrap();
        let hash = [3u8; HASH_LEN];
        let task = TaskId::random();
        store
            .snapshot(&src, hash, 3, [0u8; SECRET_LEN], task.clone())
            .unwrap();
        let path = store.get(&hash).unwrap().path.clone();
        assert!(path.exists());

        store.evict_task(&task);
        assert!(store.get(&hash).is_none());
        assert!(!path.exists(), "spool file must be unlinked on evict");

        fs::remove_dir_all(&src_dir).ok();
    }
}
