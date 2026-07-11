//! Session state file — the agent-square daemon writes its
//! view of the mesh into this file so external tools (e.g. a shell
//! statusline) and the `/square:*` skills can render the current mesh,
//! nickname, and participant count with a plain local file read. No
//! IPC, no gossip, no subprocess.
//!
//! The daemon is the **sole writer**: the `/square:*` skills are
//! read-only and never touch this file. The daemon owns every key —
//! `square`, `name`, `nickname`, `pid`, `ready`, `participant_count`,
//! `last_updated` — and writes a fresh, complete document on each update
//! (no read-merge: there are no foreign keys to preserve).
//!
//! `ready` is `false` at the early identity write and flips to `true`
//! once the event loop is serving IPC, so a reader (e.g. `agent-square ready`)
//! can gate on it rather than on the file's mere existence.
//! `participant_count` is the total number of agents in the mesh,
//! including self. `last_updated` is a unix timestamp the daemon
//! refreshes on a fixed heartbeat (see `tuning::STATE_REFRESH_SECS`)
//! even when membership is unchanged, so a reader can treat a fresh
//! value as a liveness signal. `pid` is the daemon's own process id —
//! what lets `agent-square leave`/`agent-square session` map a state file back to a
//! running daemon (and, via its ancestry, to the agent session that
//! spawned it).
//!
//! File shape (keys are serialized in sorted order — `serde_json::Map` is a
//! `BTreeMap` here, no `preserve_order` feature):
//! ```json
//! {"last_updated":1776720604,"name":"cool-team","nickname":"treat-empire","participant_count":3,"pid":34299,"ready":true,"square":"💬..."}
//! ```
//!
//! Writes are atomic (tempfile + rename on the same filesystem), so a
//! reader that opens the file mid-update always sees a consistent JSON
//! document, and each write fully replaces any existing file.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::protocol::mesh::MeshName;
use crate::protocol::{MeshId, Nickname};
use crate::util::clock;

/// Owns a session state file for the lifetime of a daemon process.
/// Drop removes the file (best effort), but because the daemon exits
/// via `std::process::exit`, callers must also call `remove()`
/// explicitly before exiting on SIGINT/SIGTERM.
#[derive(Debug)]
pub struct StateFile {
    path: PathBuf,
    mesh: String,
    name: String,
    nickname: String,
    /// The `--a2a-serve` port + bearer token, when the binding is on. The
    /// token is why every write chmods the file to 0o600.
    a2a: std::sync::Mutex<Option<(u16, String)>>,
}

impl StateFile {
    pub(crate) fn new(path: PathBuf, mesh: &MeshId, nickname: &Nickname, name: &MeshName) -> Self {
        Self {
            path,
            mesh: mesh.as_str().to_string(),
            name: name.as_str().to_string(),
            nickname: nickname.as_str().to_string(),
            a2a: std::sync::Mutex::new(None),
        }
    }

    /// Note the bound `--a2a-serve` port + bearer token so subsequent writes
    /// publish them (`a2a_port` / `a2a_token`) for local A2A clients.
    /// # Panics
    /// Panics if an internal invariant is violated.
    pub fn set_a2a(&self, port: u16, token: String) {
        *self.a2a.lock().expect("state-file a2a mutex not poisoned") = Some((port, token));
    }

    /// Write a fresh, complete state document — `square`, `name`,
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
        // The state file carries the full mesh id (and, with `--a2a-serve`, the
        // bearer token). It is born 0o600 below, but the enclosing directory must
        // also be private. `ensure_parent_private` validates the base and fails
        // closed when the target is under it (the default path, and the plugin's
        // `--state-file /tmp/agent-square-<uid>/sessions/...`), and just creates
        // the parent for an override that points elsewhere.
        crate::util::ensure_parent_private(&self.path)?;
        let mut obj = serde_json::Map::new();
        obj.insert("square".into(), self.mesh.clone().into());
        obj.insert("name".into(), self.name.clone().into());
        obj.insert("nickname".into(), self.nickname.clone().into());
        obj.insert("pid".into(), std::process::id().into());
        obj.insert("ready".into(), ready.into());
        obj.insert("participant_count".into(), participant_count.into());
        if let Some((port, token)) = self
            .a2a
            .lock()
            .expect("state-file a2a mutex not poisoned")
            .clone()
        {
            obj.insert("a2a_port".into(), port.into());
            obj.insert("a2a_token".into(), token.into());
        }
        obj.insert("last_updated".into(), clock::unix_secs().into());
        let mut json = serde_json::to_vec(&obj).map_err(std::io::Error::other)?;
        json.push(b'\n');

        // No fsync: statusline is best-effort; kernel buffer-writeback
        // is plenty. Atomic rename still gives readers a consistent view.
        let tmp = tmp_sibling(&self.path);
        // 0o600 at create, not chmod-after: with `--a2a-serve` the file
        // carries the bearer token, and a create-then-chmod sequence leaves a
        // window where the token is written at the umask-default mode (often
        // world-readable) — a local co-tenant polling the predictable tmp
        // path could open an fd during that window (POSIX checks perms at
        // open, not per read). Being born at 0o600 closes the window; the
        // uniform mode also keeps the common (no-token) case indistinguishable.
        let mut file = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&tmp)?
            }
            #[cfg(not(unix))]
            {
                std::fs::File::create(&tmp)?
            }
        };
        file.write_all(&json)?;
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

/// What the `agent-square ready` gate needs out of a state file on every poll: whether
/// the daemon is serving (`ready`) and how fresh that claim is (`last_updated`,
/// unix seconds — the daemon rewrites it on a fixed heartbeat, so a stale
/// `ready: true` left by a prior daemon killed with SIGKILL is rejected). The
/// session identity is read separately ([`read_identity`]) only on the success
/// path, so the poll loop allocates nothing beyond these two fields.
#[derive(Debug)]
pub struct ReadySnapshot {
    pub ready: bool,
    pub last_updated: u64,
}

/// Best-effort session identity for `agent-square ready --output json`. Each field is
/// `None` when the state file is unreadable, not JSON, or predates that field
/// — so the JSON the gate prints carries only the keys actually present.
#[derive(Debug)]
pub struct SessionIdentity {
    pub mesh: Option<String>,
    pub name: Option<String>,
    pub nickname: Option<String>,
}

/// Read the session identity from the state file. Best-effort: a read/parse
/// failure yields all-`None` rather than an error, since identity is a
/// convenience on top of the gate, not part of what it blocks on. Called once,
/// on success — never in the poll loop (see [`ReadySnapshot`]).
#[must_use]
pub fn read_identity(path: &Path) -> SessionIdentity {
    let parsed: Option<serde_json::Value> = std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok());
    let field = |key: &str| {
        parsed
            .as_ref()
            .and_then(|doc| doc.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    SessionIdentity {
        mesh: field("square"),
        name: field("name"),
        nickname: field("nickname"),
    }
}

/// One running daemon as seen through its state file — what `agent-square leave` /
/// `agent-square session` need to map the file back to a live process and decide
/// whether the calling session owns it. Every field is `Option`: a file
/// written by an older binary predates `pid`, and discovery must still be
/// able to *report* such an entry rather than error on it.
#[derive(Debug)]
pub struct SessionEntry {
    pub mesh: Option<String>,
    pub name: Option<String>,
    pub nickname: Option<String>,
    pub pid: Option<u32>,
}

/// Read a state file as a [`SessionEntry`]. Best-effort: `None` only when
/// the file is unreadable or not a JSON object — a *degenerate* object still
/// yields an entry (all-`None` fields) so callers can decide per-field.
pub fn read_session_entry(path: &Path) -> Option<SessionEntry> {
    let parsed: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())?;
    parsed.as_object()?;
    let text = |key: &str| {
        parsed
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    Some(SessionEntry {
        mesh: text("square"),
        name: text("name"),
        nickname: text("nickname"),
        pid: parsed
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
    })
}

/// Read the readiness fields from the state file at `path`. `Ok(None)` when
/// the file does not exist yet (the daemon has not written it — a caller
/// polling for readiness should retry). `Ok(Some)` once it carries a valid
/// `ready` + `last_updated`.
///
/// # Errors
/// The file exists but is not valid JSON, or is missing the `ready` /
/// `last_updated` fields.
pub fn read_snapshot(path: &Path) -> Result<Option<ReadySnapshot>> {
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

    use super::{MeshId, MeshName, Nickname, StateFile, clock};

    fn name(value: &str) -> MeshName {
        MeshName::new(value).expect("valid mesh name")
    }

    fn unique_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agent-square-state-test-{}-{}-{}.json",
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
            &MeshId::from("💬abcd"),
            &Nickname::from("treat-empire"),
            &name("cool-team"),
        );
        state_file.write(3, true);
        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["square"], "💬://abcd");
        assert_eq!(parsed["name"], "cool-team");
        assert_eq!(parsed["nickname"], "treat-empire");
        assert_eq!(parsed["ready"], true);
        assert_eq!(parsed["participant_count"], 3);
        assert_eq!(parsed["pid"], std::process::id());
        assert!(parsed["last_updated"].is_number());
        state_file.remove();
    }

    #[test]
    fn overwrites_on_subsequent_writes() {
        let path = unique_path("overwrite");
        let state_file = StateFile::new(
            path.clone(),
            &MeshId::from("💬xyzw"),
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
            &MeshId::from("💬test"),
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
                &MeshId::from("💬test"),
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
            "agent-square-state-test-parent-{}",
            std::process::id()
        ));
        path.push("sessions");
        path.push("x.json");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let state_file = StateFile::new(
            path.clone(),
            &MeshId::from("💬test"),
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
            &MeshId::from("💬fresh"),
            &Nickname::from("swift-cedar"),
            &name("cool-team"),
        );
        state_file.write(3, true);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["square"], "💬://fresh");
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
            &MeshId::from("💬round"),
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
    fn read_identity_carries_session_fields() {
        let path = unique_path("identity");
        let state_file = StateFile::new(
            path.clone(),
            &MeshId::from("💬round"),
            &Nickname::from("treat-empire"),
            &name("cool-team"),
        );
        state_file.write(2, true);
        let identity = super::read_identity(&path);
        assert_eq!(identity.mesh.as_deref(), Some("💬://round"));
        assert_eq!(identity.name.as_deref(), Some("cool-team"));
        assert_eq!(identity.nickname.as_deref(), Some("treat-empire"));
        state_file.remove();
    }

    #[test]
    fn read_session_entry_round_trips_write() {
        let path = unique_path("session-entry");
        let state_file = StateFile::new(
            path.clone(),
            &MeshId::from("💬round"),
            &Nickname::from("treat-empire"),
            &name("cool-team"),
        );
        state_file.write(2, true);
        let entry = super::read_session_entry(&path).expect("present");
        assert_eq!(entry.mesh.as_deref(), Some("💬://round"));
        assert_eq!(entry.name.as_deref(), Some("cool-team"));
        assert_eq!(entry.nickname.as_deref(), Some("treat-empire"));
        assert_eq!(entry.pid, Some(std::process::id()));
        state_file.remove();
    }

    #[test]
    fn read_session_entry_tolerates_a_pre_pid_file() {
        let path = unique_path("session-entry-old");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"last_updated":1,"name":"old","nickname":"n-n","participant_count":1,"ready":true,"square":"💬old"}"#,
        )
        .unwrap();
        let entry = super::read_session_entry(&path).expect("present");
        assert_eq!(entry.mesh.as_deref(), Some("💬old"));
        assert_eq!(entry.pid, None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_session_entry_is_none_on_missing_or_malformed() {
        let missing = unique_path("session-entry-missing");
        assert!(super::read_session_entry(&missing).is_none());
        let malformed = unique_path("session-entry-malformed");
        std::fs::create_dir_all(malformed.parent().unwrap()).unwrap();
        std::fs::write(&malformed, b"not json").unwrap();
        assert!(super::read_session_entry(&malformed).is_none());
        let _ = std::fs::remove_file(&malformed);
    }

    #[test]
    fn read_identity_is_all_none_on_a_missing_file() {
        // Best-effort: a missing/identity-less file yields all-None, so the
        // gate's JSON omits the keys rather than printing nulls.
        let path = unique_path("identity-missing");
        let identity = super::read_identity(&path);
        assert!(identity.mesh.is_none());
        assert!(identity.name.is_none());
        assert!(identity.nickname.is_none());
    }

    #[test]
    fn read_snapshot_errors_on_malformed() {
        let path = unique_path("malformed");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Not JSON.
        std::fs::write(&path, b"not json").unwrap();
        assert!(super::read_snapshot(&path).is_err());
        // JSON object missing the `ready` / `last_updated` fields the gate needs.
        std::fs::write(&path, r#"{"square":"💬x","name":"n"}"#).unwrap();
        assert!(super::read_snapshot(&path).is_err());
        // Has `ready` but still missing `last_updated`.
        std::fs::write(&path, r#"{"ready":true,"square":"💬round"}"#).unwrap();
        assert!(super::read_snapshot(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
