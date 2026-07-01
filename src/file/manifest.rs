//! The delta manifest — a compact, self-delimiting list of the files a peer
//! already has, so the producer sends only what is missing or has changed.
//! Wire layout: `entry_count(u32 LE)`, then per entry `path_len(u16 LE) ‖
//! path(utf8, `/`-separated) ‖ size(u64 LE) ‖ hash(32, sha256)`.

use std::collections::HashMap;

use anyhow::{Context, Result};

/// Length of a file's content hash (sha256).
pub(super) const HASH_LEN: usize = 32;

/// One file in a manifest: its path relative to the tree root, byte length, and
/// content hash. `(size, hash)` is the change key — the producer re-sends a file
/// only when the consumer lacks the path or reports a different `(size, hash)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) rel_path: String,
    pub(super) size: u64,
    pub(super) hash: [u8; HASH_LEN],
}

/// A set of [`Entry`]s describing a file or directory tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Manifest {
    pub(super) entries: Vec<Entry>,
}

impl Manifest {
    pub(super) fn encode(&self) -> Vec<u8> {
        let count = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
        let mut out = Vec::with_capacity(4 + self.entries.len() * (2 + HASH_LEN + 8));
        out.extend_from_slice(&count.to_le_bytes());
        for entry in self.entries.iter().take(count as usize) {
            let path = entry.rel_path.as_bytes();
            // `walk::scan` bails on any path over `u16::MAX`, so this never truncates.
            let path_len = u16::try_from(path.len()).unwrap_or(u16::MAX);
            out.extend_from_slice(&path_len.to_le_bytes());
            out.extend_from_slice(&path[..usize::from(path_len)]);
            out.extend_from_slice(&entry.size.to_le_bytes());
            out.extend_from_slice(&entry.hash);
        }
        out
    }

    /// Decode a manifest, validating every length against the remaining bytes.
    ///
    /// # Errors
    /// Truncated input or a non-UTF-8 path.
    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        let count = u32::from_le_bytes(
            bytes
                .get(..4)
                .context("manifest missing entry count")?
                .try_into()
                .expect("4-byte slice"),
        );
        let mut pos = 4;
        // Do NOT pre-allocate from `count` — it is attacker-controlled and a
        // 4-byte header can claim billions of entries. The loop grows the Vec and
        // bails as soon as the (length-capped) bytes run out.
        let mut entries = Vec::new();
        for _ in 0..count {
            let path_len = usize::from(u16::from_le_bytes(
                bytes
                    .get(pos..pos + 2)
                    .context("manifest truncated at path length")?
                    .try_into()
                    .expect("2-byte slice"),
            ));
            pos += 2;
            let path = bytes
                .get(pos..pos + path_len)
                .context("manifest truncated in path")?;
            let rel_path = String::from_utf8(path.to_vec()).context("manifest path is not UTF-8")?;
            pos += path_len;
            let size = u64::from_le_bytes(
                bytes
                    .get(pos..pos + 8)
                    .context("manifest truncated at size")?
                    .try_into()
                    .expect("8-byte slice"),
            );
            pos += 8;
            let mut hash = [0u8; HASH_LEN];
            hash.copy_from_slice(
                bytes
                    .get(pos..pos + HASH_LEN)
                    .context("manifest truncated in hash")?,
            );
            pos += HASH_LEN;
            entries.push(Entry {
                rel_path,
                size,
                hash,
            });
        }
        Ok(Self { entries })
    }

    /// A path → entry index for diffing against another tree.
    pub(super) fn by_path(&self) -> HashMap<&str, &Entry> {
        self.entries
            .iter()
            .map(|entry| (entry.rel_path.as_str(), entry))
            .collect()
    }

    /// Our entries the peer is missing or holds an outdated copy of — i.e. the
    /// files to actually send. `theirs` is the peer's manifest of what it has.
    pub(super) fn diff<'a>(&'a self, theirs: &Manifest) -> Vec<&'a Entry> {
        let have = theirs.by_path();
        self.entries
            .iter()
            .filter(|entry| {
                have.get(entry.rel_path.as_str())
                    .is_none_or(|their| their.size != entry.size || their.hash != entry.hash)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, Manifest};

    fn entry(path: &str, size: u64, byte: u8) -> Entry {
        Entry {
            rel_path: path.to_owned(),
            size,
            hash: [byte; 32],
        }
    }

    #[test]
    fn round_trips() {
        let manifest = Manifest {
            entries: vec![entry("a.txt", 10, 1), entry("dir/b.bin", 4096, 2)],
        };
        let decoded = Manifest::decode(&manifest.encode()).expect("decode");
        assert_eq!(decoded, manifest);
    }

    #[test]
    fn empty_round_trips() {
        let decoded = Manifest::decode(&Manifest::default().encode()).expect("decode");
        assert!(decoded.entries.is_empty());
    }

    #[test]
    fn decode_rejects_truncation() {
        let mut bytes = Manifest {
            entries: vec![entry("a", 1, 1)],
        }
        .encode();
        bytes.pop();
        assert!(Manifest::decode(&bytes).is_err());
    }

    #[test]
    fn diff_selects_missing_and_changed() {
        let source = Manifest {
            entries: vec![
                entry("keep", 10, 1),
                entry("changed", 10, 2),
                entry("new", 5, 3),
            ],
        };
        let theirs = Manifest {
            entries: vec![
                entry("keep", 10, 1),         // identical → skip
                entry("changed", 10, 9),      // same size, different hash → send
                entry("extra-on-theirs", 1, 4), // not ours → ignored
            ],
        };
        let to_send: Vec<&str> = source
            .diff(&theirs)
            .iter()
            .map(|entry| entry.rel_path.as_str())
            .collect();
        assert_eq!(to_send, vec!["changed", "new"]);
    }

    #[test]
    fn diff_detects_size_change_alone() {
        let source = Manifest {
            entries: vec![entry("f", 20, 1)],
        };
        let theirs = Manifest {
            entries: vec![entry("f", 10, 1)], // same hash byte, different size → send
        };
        assert_eq!(source.diff(&theirs).len(), 1);
    }
}
