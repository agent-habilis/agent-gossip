//! Session state file — the agent-habilis-swarm daemon merges its
//! view of the swarm into this file so external tools (e.g. a shell
//! statusline) can render the current nickname and participant count
//! with a plain local file read. No IPC, no gossip, no subprocess
//! beyond `jq`.
//!
//! The file is *shared*: the `/swarm:*` skills also write it (keys
//! `name`, `auto_reply`, `known_messages`, and transient ping
//! fields). The daemon owns only `swarm`, `nickname`,
//! `participant_count`, and `last_updated`; every write is
//! read-merge-write, so the skill-owned keys survive regardless of
//! which side wrote last.
//!
//! `participant_count` is the total number of agents in the swarm,
//! including self. `last_updated` is a unix timestamp the daemon
//! refreshes on a fixed heartbeat (see `tuning::STATE_REFRESH_SECS`)
//! even when membership is unchanged, so a reader can treat a fresh
//! value as a liveness signal.
//!
//! File shape (daemon-owned keys shown; skill keys merged alongside):
//! ```json
//! {"last_updated":1776720604,"nickname":"treat-empire","participant_count":3,"swarm":"ahs..."}
//! ```
//!
//! Writes are atomic (tempfile + rename on the same filesystem), so a
//! reader that opens the file mid-update always sees a consistent JSON
//! document.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::protocol::{Nickname, SwarmId};
use crate::util::clock;

/// Owns a session state file for the lifetime of a daemon process.
/// Drop removes the file (best effort), but because the daemon exits
/// via `std::process::exit`, callers must also call `remove()`
/// explicitly before exiting on SIGINT/SIGTERM.
pub(crate) struct StateFile {
    path: PathBuf,
    swarm: String,
    nickname: String,
}

impl StateFile {
    pub(crate) fn new(path: PathBuf, swarm: &SwarmId, nickname: &Nickname) -> Self {
        Self {
            path,
            swarm: swarm.as_str().to_string(),
            nickname: nickname.as_str().to_string(),
        }
    }

    /// Merge `participant_count` — plus `swarm`, `nickname`, and a
    /// fresh `last_updated` — atomically into the state file,
    /// preserving any keys written by the `/swarm:*` skills. Creates
    /// parent directories as needed. Errors are logged but not
    /// propagated — the statusline going stale must not crash the
    /// daemon.
    pub(crate) fn write(&self, participant_count: usize) {
        if let Err(error) = self.try_write(participant_count) {
            eprintln!("state_file: write failed: {error}");
        }
    }

    fn try_write(&self, participant_count: usize) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Read-merge-write: the `/swarm:*` skills share this file and
        // own `name`/`auto_reply`/`known_messages`. Preserve every key
        // we do not own; default to an empty object if the file is
        // absent or unparseable.
        let mut obj = std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| {
                serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&bytes).ok()
            })
            .unwrap_or_default();
        obj.insert("swarm".into(), self.swarm.clone().into());
        obj.insert("nickname".into(), self.nickname.clone().into());
        obj.insert("participant_count".into(), participant_count.into());
        obj.insert("last_updated".into(), clock::unix_secs().into());
        let mut json = serde_json::to_vec(&obj).map_err(std::io::Error::other)?;
        json.push(b'\n');

        // No fsync: statusline is best-effort; kernel buffer-writeback
        // is plenty. Atomic rename still gives readers a consistent view.
        let tmp = tmp_sibling(&self.path);
        std::fs::File::create(&tmp)?.write_all(&json)?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Delete the state file. Safe to call on a path that does not exist.
    pub(crate) fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for StateFile {
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

    use super::{Nickname, StateFile, SwarmId, clock};

    fn unique_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agent-habilis-swarm-state-test-{}-{}-{}.json",
            tag,
            std::process::id(),
            clock::unix_nanos(),
        ))
    }

    #[test]
    fn writes_expected_shape() {
        let path = unique_path("shape");
        let state_file = StateFile::new(
            path.clone(),
            &SwarmId::from("ahsabcd"),
            &Nickname::from("treat-empire"),
        );
        state_file.write(3);
        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["swarm"], "ahsabcd");
        assert_eq!(parsed["nickname"], "treat-empire");
        assert_eq!(parsed["participant_count"], 3);
        assert!(parsed["last_updated"].is_number());
        state_file.remove();
    }

    #[test]
    fn overwrites_on_subsequent_writes() {
        let path = unique_path("overwrite");
        let state_file = StateFile::new(
            path.clone(),
            &SwarmId::from("ahsxyzw"),
            &Nickname::from("swift-cedar"),
        );
        state_file.write(0);
        state_file.write(1);
        state_file.write(7);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["participant_count"], 7);
        state_file.remove();
    }

    #[test]
    fn remove_clears_file() {
        let path = unique_path("remove");
        let state_file = StateFile::new(
            path.clone(),
            &SwarmId::from("ahstest"),
            &Nickname::from("n"),
        );
        state_file.write(0);
        assert!(path.exists());
        state_file.remove();
        assert!(!path.exists());
    }

    #[test]
    fn drop_removes_file() {
        let path = unique_path("drop");
        {
            let state_file = StateFile::new(
                path.clone(),
                &SwarmId::from("ahstest"),
                &Nickname::from("n"),
            );
            state_file.write(0);
            assert!(path.exists());
        }
        assert!(!path.exists());
    }

    #[test]
    fn creates_parent_dir() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "agent-habilis-swarm-state-test-parent-{}",
            std::process::id()
        ));
        path.push("sessions");
        path.push("x.json");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let state_file = StateFile::new(
            path.clone(),
            &SwarmId::from("ahstest"),
            &Nickname::from("n"),
        );
        state_file.write(2);
        assert!(path.exists());
        state_file.remove();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn merge_preserves_foreign_keys() {
        let path = unique_path("merge");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"name":"x","auto_reply":false}"#).unwrap();
        let state_file = StateFile::new(
            path.clone(),
            &SwarmId::from("ahsmerge"),
            &Nickname::from("swift-cedar"),
        );
        state_file.write(3);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["name"], "x");
        assert_eq!(parsed["auto_reply"], false);
        assert_eq!(parsed["swarm"], "ahsmerge");
        assert_eq!(parsed["nickname"], "swift-cedar");
        assert_eq!(parsed["participant_count"], 3);
        assert!(parsed["last_updated"].is_number());
        state_file.remove();
    }
}
