use anyhow::{Context, Result, bail};

/// One directory in the shared tree (every directory, not just empty ones —
/// the consumer builds its tree directly from this list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DirEntry {
    /// `/`-separated path relative to the shared root.
    pub rel_path: String,
    pub mode: u32,
    /// Seconds since the epoch; 0 when unknown.
    pub mtime: i64,
}

/// One file in the shared tree. Its position in [`MountManifest::files`] is
/// the index READ requests address it by — no hash: bytes are fetched lazily,
/// so hashing the tree up-front would defeat the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileEntry {
    /// `/`-separated path relative to the shared root.
    pub rel_path: String,
    pub size: u64,
    pub mode: u32,
    /// Seconds since the epoch; 0 when unknown.
    pub mtime: i64,
}

/// The mount manifest: the complete tree listing a consumer turns into a
/// filesystem. Distinct from the file-transfer manifest — mount needs
/// mode/mtime and explicit dirs, and deliberately carries no content hashes.
///
/// Wire layout (little-endian):
/// `dir_count(u32) [path_len(u16) ‖ path ‖ mode(u32) ‖ mtime(i64)]…`
/// `file_count(u32) [path_len(u16) ‖ path ‖ size(u64) ‖ mode(u32) ‖ mtime(i64)]…`
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct MountManifest {
    pub dirs: Vec<DirEntry>,
    pub files: Vec<FileEntry>,
}

impl MountManifest {
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(
            &u32::try_from(self.dirs.len())
                .expect("dir count fits u32")
                .to_le_bytes(),
        );
        for dir in &self.dirs {
            encode_path(&mut out, &dir.rel_path);
            out.extend_from_slice(&dir.mode.to_le_bytes());
            out.extend_from_slice(&dir.mtime.to_le_bytes());
        }
        out.extend_from_slice(
            &u32::try_from(self.files.len())
                .expect("file count fits u32")
                .to_le_bytes(),
        );
        for file in &self.files {
            encode_path(&mut out, &file.rel_path);
            out.extend_from_slice(&file.size.to_le_bytes());
            out.extend_from_slice(&file.mode.to_le_bytes());
            out.extend_from_slice(&file.mtime.to_le_bytes());
        }
        out
    }

    /// Decode a manifest received from the producer. Incremental: every read
    /// is bounds-checked against the remaining bytes, so a hostile count can
    /// cause a decode error but never an unbounded allocation.
    ///
    /// # Errors
    /// Truncated input, a non-UTF-8 path, or trailing garbage.
    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        let mut cursor = Cursor { bytes, pos: 0 };
        let dir_count = cursor.take_u32()?;
        let mut dirs = Vec::new();
        for _ in 0..dir_count {
            let rel_path = cursor.take_path()?;
            let mode = cursor.take_u32()?;
            let mtime = cursor.take_i64()?;
            dirs.push(DirEntry {
                rel_path,
                mode,
                mtime,
            });
        }
        let file_count = cursor.take_u32()?;
        let mut files = Vec::new();
        for _ in 0..file_count {
            let rel_path = cursor.take_path()?;
            let size = cursor.take_u64()?;
            let mode = cursor.take_u32()?;
            let mtime = cursor.take_i64()?;
            files.push(FileEntry {
                rel_path,
                size,
                mode,
                mtime,
            });
        }
        if cursor.pos != bytes.len() {
            bail!("trailing bytes after the manifest");
        }
        Ok(Self { dirs, files })
    }
}

fn encode_path(out: &mut Vec<u8>, path: &str) {
    let len = u16::try_from(path.len()).expect("scan rejects paths over MAX_REL_PATH");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(path.as_bytes());
}

/// The result byte leading every READ response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadStatus {
    Ok,
    BadIndex,
    Io,
    LenOverCap,
}

impl ReadStatus {
    pub(super) fn to_byte(self) -> u8 {
        match self {
            ReadStatus::Ok => 0,
            ReadStatus::BadIndex => 1,
            ReadStatus::Io => 2,
            ReadStatus::LenOverCap => 3,
        }
    }

    pub(super) fn from_byte(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(ReadStatus::Ok),
            1 => Ok(ReadStatus::BadIndex),
            2 => Ok(ReadStatus::Io),
            3 => Ok(ReadStatus::LenOverCap),
            other => bail!("unknown read status: {other}"),
        }
    }
}

/// A bounds-checked reader over the received manifest bytes.
struct Cursor<'bytes> {
    bytes: &'bytes [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take(&mut self, len: usize) -> Result<&[u8]> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|&end| end <= self.bytes.len())
            .context("truncated manifest")?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn take_u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }

    fn take_u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn take_u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn take_i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    fn take_path(&mut self) -> Result<String> {
        let len = usize::from(self.take_u16()?);
        let raw = self.take(len)?;
        String::from_utf8(raw.to_vec()).context("non-UTF-8 path in manifest")
    }
}

#[cfg(test)]
mod tests {
    use super::{DirEntry, FileEntry, MountManifest, ReadStatus};

    fn sample() -> MountManifest {
        MountManifest {
            dirs: vec![
                DirEntry {
                    rel_path: "docs".to_owned(),
                    mode: 0o755,
                    mtime: 1_700_000_000,
                },
                DirEntry {
                    rel_path: "docs/empty".to_owned(),
                    mode: 0o700,
                    mtime: 0,
                },
            ],
            files: vec![
                FileEntry {
                    rel_path: "README.md".to_owned(),
                    size: 42,
                    mode: 0o644,
                    mtime: 1_700_000_001,
                },
                FileEntry {
                    rel_path: "docs/guide.md".to_owned(),
                    size: 0,
                    mode: 0o600,
                    mtime: -1,
                },
            ],
        }
    }

    #[test]
    fn manifest_round_trips() {
        let manifest = sample();
        let decoded = MountManifest::decode(&manifest.encode()).expect("decode");
        assert_eq!(decoded, manifest);
    }

    #[test]
    fn truncated_manifest_is_rejected() {
        let encoded = sample().encode();
        for len in 0..encoded.len() {
            assert!(
                MountManifest::decode(&encoded[..len]).is_err(),
                "prefix of {len} bytes must not decode"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut encoded = sample().encode();
        encoded.push(0);
        assert!(MountManifest::decode(&encoded).is_err());
    }

    #[test]
    fn hostile_count_fails_without_allocating() {
        // Claims u32::MAX dirs but carries none: must error, not OOM.
        let bytes = u32::MAX.to_le_bytes();
        assert!(MountManifest::decode(&bytes).is_err());
    }

    #[test]
    fn read_status_round_trips() {
        for status in [
            ReadStatus::Ok,
            ReadStatus::BadIndex,
            ReadStatus::Io,
            ReadStatus::LenOverCap,
        ] {
            assert_eq!(
                ReadStatus::from_byte(status.to_byte()).expect("round trip"),
                status
            );
        }
        assert!(ReadStatus::from_byte(9).is_err());
    }
}
