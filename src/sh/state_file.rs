use std::io::Write;
use std::path::{Path, PathBuf};

use crate::util::{clock, consts};

/// A sh session's state file — `<RUNTIME_DIR>/sh/<sh-prefix>.state.json`. The
/// prefix is what `AHSW_SH` (see [`super::ENV_SH`]) carries into the broadcast
/// shell, so anything inside derives this path from the env var alone — the
/// same id-prefix scheme `util::swarm_prefix` uses for a swarm's runtime dir.
pub(crate) fn path_for(sh_prefix: &str) -> PathBuf {
    Path::new(consts::RUNTIME_DIR)
        .join("sh")
        .join(format!("{sh_prefix}.state.json"))
}

/// Owns a sh session's state file so a prompt inside the broadcast shell (e.g.
/// a starship segment) can render the live viewer count with a plain local
/// file read — no IPC, no subprocess. The producer is the sole writer; each
/// write atomically replaces the whole document (tempfile + rename). Drop
/// removes the file, but the producer's clean path exits via
/// `std::process::exit`, which skips Drop — callers must also call `remove()`
/// explicitly.
///
/// File shape (keys serialized in sorted order; `swarm` present only when the
/// session was started with `--swarm`):
/// ```json
/// {"last_updated":1776720604,"swarm":"🐝...","viewer_count":2}
/// ```
pub(crate) struct ShStateFile {
    path: PathBuf,
    sh_prefix: String,
    swarm: Option<String>,
    // Skips writes when the count is unchanged, so the serve loop can call
    // `update` unconditionally after every arm that may have mutated viewers
    // without steady-state output causing any I/O.
    last_written: Option<usize>,
}

impl ShStateFile {
    pub(crate) fn new(path: PathBuf, sh_prefix: &str, swarm: Option<&str>) -> Self {
        Self {
            path,
            sh_prefix: sh_prefix.to_owned(),
            swarm: swarm.map(str::to_owned),
            last_written: None,
        }
    }

    /// The value `AHSW_SH` carries into the shell — kept here so the env
    /// injection and the state file can never disagree on the key.
    pub(crate) fn sh_prefix(&self) -> &str {
        &self.sh_prefix
    }

    /// Write the viewer count atomically, fully replacing any existing file.
    /// A no-op when the count is unchanged since the last successful write.
    /// Errors are logged but not propagated — the prompt going stale must not
    /// kill the broadcast.
    pub(crate) fn update(&mut self, viewer_count: usize) {
        if self.last_written == Some(viewer_count) {
            return;
        }
        match self.try_write(viewer_count) {
            Ok(()) => self.last_written = Some(viewer_count),
            Err(error) => eprintln!("sh state_file: write failed: {error}"),
        }
    }

    fn try_write(&self, viewer_count: usize) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut obj = serde_json::Map::new();
        if let Some(swarm) = &self.swarm {
            obj.insert("swarm".into(), swarm.clone().into());
        }
        obj.insert("viewer_count".into(), viewer_count.into());
        obj.insert("last_updated".into(), clock::unix_secs().into());
        let mut json = serde_json::to_vec(&obj).map_err(std::io::Error::other)?;
        json.push(b'\n');

        // No fsync: the prompt is best-effort; kernel buffer-writeback is
        // plenty. Atomic rename still gives readers a consistent view.
        let tmp = tmp_sibling(&self.path);
        std::fs::File::create(&tmp)?.write_all(&json)?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Delete the state file. Safe to call on a path that does not exist.
    pub(crate) fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for ShStateFile {
    fn drop(&mut self) {
        self.remove();
    }
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    file_name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::ShStateFile;
    use crate::util::clock;

    fn unique_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agent-habilis-swarm-sh-state-test-{}-{}-{}.json",
            tag,
            std::process::id(),
            clock::unix_nanos(),
        ))
    }

    #[test]
    fn writes_expected_shape() {
        let path = unique_path("shape");
        let mut state_file = ShStateFile::new(path.clone(), "abcdef0123456789", Some("🐝abcd"));
        state_file.update(2);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["swarm"], "🐝abcd");
        assert_eq!(parsed["viewer_count"], 2);
        assert!(parsed["last_updated"].is_number());
        state_file.remove();
    }

    #[test]
    fn omits_swarm_when_none() {
        let path = unique_path("no-swarm");
        let mut state_file = ShStateFile::new(path.clone(), "abcdef0123456789", None);
        state_file.update(0);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(parsed.get("swarm").is_none());
        assert_eq!(parsed["viewer_count"], 0);
        state_file.remove();
    }

    #[test]
    fn dedups_unchanged_count() {
        let path = unique_path("dedup");
        let mut state_file = ShStateFile::new(path.clone(), "abcdef0123456789", None);
        state_file.update(1);
        // Deleting out from under it makes a skipped write observable: an
        // unchanged count must not recreate the file, a changed one must.
        std::fs::remove_file(&path).unwrap();
        state_file.update(1);
        assert!(!path.exists(), "unchanged count must skip the write");
        state_file.update(2);
        assert!(path.exists(), "changed count must write");
        state_file.remove();
    }

    #[test]
    fn remove_clears_file() {
        let path = unique_path("remove");
        let mut state_file = ShStateFile::new(path.clone(), "abcdef0123456789", None);
        state_file.update(0);
        assert!(path.exists());
        state_file.remove();
        assert!(!path.exists());
    }

    #[test]
    fn drop_removes_file() {
        let path = unique_path("drop");
        {
            let mut state_file = ShStateFile::new(path.clone(), "abcdef0123456789", None);
            state_file.update(0);
            assert!(path.exists());
        }
        assert!(!path.exists());
    }

    #[test]
    fn creates_parent_dir() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "agent-habilis-swarm-sh-state-test-parent-{}",
            std::process::id()
        ));
        path.push("sh");
        path.push("x.state.json");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let mut state_file = ShStateFile::new(path.clone(), "abcdef0123456789", None);
        state_file.update(1);
        assert!(path.exists());
        state_file.remove();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn path_for_derives_the_documented_scheme() {
        let path = super::path_for("abcdef0123456789");
        assert_eq!(
            path,
            std::path::Path::new("/tmp/agent-habilis/swarm/sh/abcdef0123456789.state.json")
        );
    }
}
