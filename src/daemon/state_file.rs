//! Session state file — the agent-habilis-swarm daemon writes its
//! view of the swarm into this file so external tools (e.g. a shell
//! statusline) and the `/swarm:*` skills can render the current swarm,
//! nickname, and participant count with a plain local file read. No
//! IPC, no gossip, no subprocess.
//!
//! The daemon is the **sole writer**: the `/swarm:*` skills are
//! read-only and never touch this file. The daemon owns every key —
//! `swarm`, `name`, `nickname`, `ready`, `participant_count`,
//! `last_updated` — and writes a fresh, complete document on each update
//! (no read-merge: there are no foreign keys to preserve).
//!
//! `ready` is `false` at the early identity write and flips to `true`
//! once the event loop is serving IPC, so a reader (e.g. `ahs ready`)
//! can gate on it rather than on the file's mere existence.
//! `participant_count` is the total number of agents in the swarm,
//! including self. `last_updated` is a unix timestamp the daemon
//! refreshes on a fixed heartbeat (see `tuning::STATE_REFRESH_SECS`)
//! even when membership is unchanged, so a reader can treat a fresh
//! value as a liveness signal.
//!
//! File shape (keys are serialized in sorted order — `serde_json::Map` is a
//! `BTreeMap` here, no `preserve_order` feature):
//! ```json
//! {"last_updated":1776720604,"name":"cool-team","nickname":"treat-empire","participant_count":3,"ready":true,"swarm":"ahs..."}
//! ```
//!
//! Writes are atomic (tempfile + rename on the same filesystem), so a
//! reader that opens the file mid-update always sees a consistent JSON
//! document, and each write fully replaces any existing file.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::protocol::swarm::SwarmName;
use crate::protocol::{Nickname, SwarmId};
use crate::util::clock;

/// Owns a session state file for the lifetime of a daemon process.
/// Drop removes the file (best effort), but because the daemon exits
/// via `std::process::exit`, callers must also call `remove()`
/// explicitly before exiting on SIGINT/SIGTERM.
pub(crate) struct StateFile {
    path: PathBuf,
    swarm: String,
    name: String,
    nickname: String,
}

impl StateFile {
    pub(crate) fn new(
        path: PathBuf,
        swarm: &SwarmId,
        nickname: &Nickname,
        name: &SwarmName,
    ) -> Self {
        Self {
            path,
            swarm: swarm.as_str().to_string(),
            name: name.as_str().to_string(),
            nickname: nickname.as_str().to_string(),
        }
    }

    /// Write a fresh, complete state document — `swarm`, `name`,
    /// `nickname`, `ready`, `participant_count`, and a fresh
    /// `last_updated` — atomically, fully replacing any existing file.
    /// `ready` is `false` at the early identity write and `true` once the
    /// daemon's event loop is serving IPC, so a reader can gate on it. The
    /// daemon is the sole writer (skills are read-only), so there is
    /// nothing to merge. Creates parent directories as needed. Errors are
    /// logged but not propagated — the statusline going stale must not
    /// crash the daemon.
    pub(crate) fn write(&self, participant_count: usize, ready: bool) {
        if let Err(error) = self.try_write(participant_count, ready) {
            eprintln!("state_file: write failed: {error}");
        }
    }

    fn try_write(&self, participant_count: usize, ready: bool) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut obj = serde_json::Map::new();
        obj.insert("swarm".into(), self.swarm.clone().into());
        obj.insert("name".into(), self.name.clone().into());
        obj.insert("nickname".into(), self.nickname.clone().into());
        obj.insert("ready".into(), ready.into());
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

/// What a reader (the `ahs ready` gate) needs out of a state file: whether
/// the daemon is serving (`ready`) and how fresh that claim is
/// (`last_updated`, unix seconds — the daemon rewrites it on a fixed
/// heartbeat, so a gate can reject a stale `ready: true` left by a prior
/// daemon killed with SIGKILL). The identity fields (`swarm`/`name`/`nickname`)
/// are not read here — the binary doesn't consume them (the skill reads them
/// straight off the JSON), and `ready`/`last_updated` already fail-closed on a
/// torn or foreign file, so re-validating identity on every poll would be
/// wasted work.
pub(crate) struct ReadySnapshot {
    pub ready: bool,
    pub last_updated: u64,
}

/// Read the readiness fields from the state file at `path`. `Ok(None)` when
/// the file does not exist yet (the daemon has not written it — a caller
/// polling for readiness should retry). `Ok(Some)` once it carries a valid
/// `ready` + `last_updated`.
///
/// # Errors
/// The file exists but is not valid JSON, or is missing the `ready` /
/// `last_updated` fields.
pub(crate) fn read_snapshot(path: &Path) -> Result<Option<ReadySnapshot>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let value: serde_json::Value =
        serde_json::from_str(&contents).with_context(|| format!("parsing {}", path.display()))?;
    let ready = value["ready"]
        .as_bool()
        .with_context(|| format!("state file {} missing bool field `ready`", path.display()))?;
    let last_updated = value["last_updated"].as_u64().with_context(|| {
        format!(
            "state file {} missing integer field `last_updated`",
            path.display()
        )
    })?;
    Ok(Some(ReadySnapshot {
        ready,
        last_updated,
    }))
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

    use super::{Nickname, StateFile, SwarmId, SwarmName, clock};

    fn name(value: &str) -> SwarmName {
        SwarmName::new(value).expect("valid swarm name")
    }

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
            &name("cool-team"),
        );
        state_file.write(3, true);
        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["swarm"], "ahsabcd");
        assert_eq!(parsed["name"], "cool-team");
        assert_eq!(parsed["nickname"], "treat-empire");
        assert_eq!(parsed["ready"], true);
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
            &name("cool-team"),
        );
        state_file.write(0, false);
        state_file.write(1, true);
        state_file.write(7, true);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["participant_count"], 7);
        assert_eq!(parsed["ready"], true);
        state_file.remove();
    }

    #[test]
    fn remove_clears_file() {
        let path = unique_path("remove");
        let state_file = StateFile::new(
            path.clone(),
            &SwarmId::from("ahstest"),
            &Nickname::from("n"),
            &name("cool-team"),
        );
        state_file.write(0, false);
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
                &name("cool-team"),
            );
            state_file.write(0, false);
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
            &name("cool-team"),
        );
        state_file.write(2, true);
        assert!(path.exists());
        state_file.remove();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn replaces_existing_file() {
        let path = unique_path("replace");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A pre-existing file with stale/foreign keys is fully replaced —
        // the daemon is the sole writer, so nothing is merged or preserved.
        std::fs::write(&path, br#"{"name":"stale","auto_reply":false,"junk":1}"#).unwrap();
        let state_file = StateFile::new(
            path.clone(),
            &SwarmId::from("ahsfresh"),
            &Nickname::from("swift-cedar"),
            &name("cool-team"),
        );
        state_file.write(3, true);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["swarm"], "ahsfresh");
        assert_eq!(parsed["name"], "cool-team");
        assert_eq!(parsed["nickname"], "swift-cedar");
        assert_eq!(parsed["participant_count"], 3);
        assert!(parsed["last_updated"].is_number());
        assert!(parsed.get("auto_reply").is_none());
        assert!(parsed.get("junk").is_none());
        state_file.remove();
    }

    #[test]
    fn read_snapshot_round_trips_write() {
        let path = unique_path("snapshot");
        let state_file = StateFile::new(
            path.clone(),
            &SwarmId::from("ahsround"),
            &Nickname::from("treat-empire"),
            &name("cool-team"),
        );
        // Missing file → None (the caller polls and retries).
        assert!(super::read_snapshot(&path).unwrap().is_none());

        state_file.write(1, false);
        let snap = super::read_snapshot(&path).unwrap().expect("present");
        assert!(!snap.ready, "wrote ready:false");
        assert!(snap.last_updated > 0, "last_updated populated");

        state_file.write(1, true);
        assert!(
            super::read_snapshot(&path).unwrap().expect("present").ready,
            "wrote ready:true"
        );
        state_file.remove();
    }

    #[test]
    fn read_snapshot_errors_on_malformed() {
        let path = unique_path("malformed");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Not JSON.
        std::fs::write(&path, b"not json").unwrap();
        assert!(super::read_snapshot(&path).is_err());
        // JSON object missing the `ready` / `last_updated` fields the gate needs.
        std::fs::write(&path, br#"{"swarm":"ahsx","name":"n"}"#).unwrap();
        assert!(super::read_snapshot(&path).is_err());
        // Has `ready` but still missing `last_updated`.
        std::fs::write(&path, br#"{"ready":true,"swarm":"ahsround"}"#).unwrap();
        assert!(super::read_snapshot(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
