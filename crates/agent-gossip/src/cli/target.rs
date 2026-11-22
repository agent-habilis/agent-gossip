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
    pub(super) nickname: Option<String>,
    /// The author key of the daemon, hex: the slot of its meta entry. A skill that lost its
    /// context recovers it from the session report.
    pub(super) pubkey: Option<String>,
    /// The raw topic string (topic gossips only) — what the leave report
    /// echoes instead of the lossy derived `name`.
    pub(super) topic: Option<String>,
    pub(super) pid: u32,
    /// The agent process a detached daemon (`--owner-pid`) answers to. Such a
    /// daemon is no descendant of it, so ancestry alone cannot match.
    pub(super) owner_pid: Option<u32>,
}
