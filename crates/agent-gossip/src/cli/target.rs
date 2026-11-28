use std::path::PathBuf;

/// One live daemon on this machine, resolved from its record in the registry. `mesh`
/// and `pid` are required to act on an entry; `name`/`nickname`/`topic` are
/// carried for reporting only.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(super) path: PathBuf,
    /// The lock file of the member in the registry: its record is the proof that the daemon runs, and
    /// `leave` waits on the lock until the daemon has ended.
    pub(super) lock: PathBuf,
    pub(super) mesh: String,
    pub(super) name: Option<String>,
    pub(super) nickname: String,
    /// The author key of the daemon, hex: the slot of its meta entry. A skill that lost its
    /// context recovers it from the session report.
    pub(super) pubkey: Option<String>,
    /// The member key of the daemon, read when discovery found it. A daemon that stops removes its state
    /// file, so the `leave` report cannot read the key after the stop.
    pub(super) member: serde_json::Value,
    /// The raw topic string (topic gossips only) — what the leave report
    /// echoes instead of the lossy derived `name`.
    pub(super) topic: Option<String>,
    pub(super) pid: u32,
    /// The start time of the process, from the record that discovery read. A pid that a new process
    /// takes later has another start, so a signal needs both.
    pub(super) start: u64,
    /// The agent process a detached daemon (`--owner-pid`) answers to. Such a
    /// daemon is no descendant of it, so ancestry alone cannot match.
    pub(super) owner_pid: Option<u32>,
}
