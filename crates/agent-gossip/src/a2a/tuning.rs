//! Process tuning for the parts of the A2A layer the engine does not own: the
//! task state machine and the long-poll park.
//!
//! Mirrors `agent_habilis_mesh::util::tuning` in shape — a `const` default per
//! knob, one `OnceLock` installed from the hidden CLI flags — but deliberately
//! separate. These govern behavior implemented entirely in `a2a`: the engine has
//! no task state machine and no poll waiters, so it must not carry the dials for
//! them. No environment variables; edit + commit to change a default, or pass
//! the hidden flag.

use std::sync::OnceLock;

/// Idle ceiling for one A2A task before the daemon fails it (seconds).
///
/// Covers the worst case a *skill* can sit silent between legs. The keepalive
/// below is what keeps a genuinely-working task from tripping it.
pub(crate) const TASK_TIMEOUT_SECS: u64 = 120;

/// How often the ball-owner's daemon emits a task keepalive (seconds).
pub(crate) const TASK_KEEPALIVE_SECS: u64 = 30;

/// Longest the daemon auto-covers a silent task with keepalives before it stops
/// and lets [`TASK_TIMEOUT_SECS`] run (seconds). Bounds how long a wedged skill
/// can be kept alive by plumbing alone.
pub(crate) const TASK_KEEPALIVE_MAX_SECS: u64 = 120;

/// Longest a blocking `poll` / `fetch_messages` parks before returning empty
/// (milliseconds). A ceiling, not a timeout the caller sees as an error: the
/// client re-issues.
pub(crate) const LONGPOLL_MAX_MS: u64 = 60_000;

/// Max messages a single `poll` / `fetch_messages` returns — a **fixed** IPC
/// contract (a long-poll client can't know the daemon's configured log size, so
/// the read cap can't depend on it). At the engine's default message-log size
/// this equals the log, so `poll` returns everything; a larger configured log
/// just means `poll` surfaces the most-recent window.
pub(crate) const POLL_RESPONSE_MAX_MSGS: usize = 1000;

/// Capacity of the daemon-local **surfaced-events** ring — the seq-ordered
/// history `poll` / `fetch_messages` drain. Distinct from the engine's message
/// log: that is the cross-node anti-entropy buffer (evicted by a deterministic
/// `eviction_key`); this is a *local* record of what was surfaced to the
/// operator/agent (msg + presence + task legs **and** the transient events —
/// `ping_report`, `peer_timeout`/`return`, `task_timeout`, `fork` — that never
/// entered the message log). Oldest-drop on overflow.
///
/// Sized **equal to** [`POLL_RESPONSE_MAX_MSGS`] so the ring never holds more
/// than a single `poll` returns: ring eviction and the response cap coincide, so
/// the response cap is a no-op in the steady state and no surfaced event is ever
/// dropped *after* `since()` selected it but *before* the client could read it
/// (a larger ring would silently lose the oldest events on a full first poll,
/// with the client's cursor advancing past them).
pub(crate) const SURFACED_EVENTS_CAP: usize = POLL_RESPONSE_MAX_MSGS;

/// Max concurrent parked long-poll waiters per daemon. A blocking `poll` /
/// `fetch_messages` registers a waiter when the buffer is empty; over this cap
/// the read degrades to an immediate (empty) return rather than parking, so the
/// registry can never grow without bound (the bounded-everything discipline).
pub(crate) const POLL_WAITERS_CAP: usize = 64;

/// The runtime-varied knobs, installed once at startup from the hidden flags.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tuning {
    pub(crate) task_timeout_secs: u64,
    pub(crate) task_keepalive_secs: u64,
    pub(crate) task_keepalive_max_secs: u64,
    pub(crate) longpoll_max_ms: u64,
}

impl Tuning {
    pub(crate) const DEFAULTS: Self = Self {
        task_timeout_secs: TASK_TIMEOUT_SECS,
        task_keepalive_secs: TASK_KEEPALIVE_SECS,
        task_keepalive_max_secs: TASK_KEEPALIVE_MAX_SECS,
        longpoll_max_ms: LONGPOLL_MAX_MS,
    };
}

static TUNING: OnceLock<Tuning> = OnceLock::new();

/// Install the process tuning, once. A second call is ignored — same
/// first-wins contract as the engine's.
pub(crate) fn init(tuning: Tuning) {
    let _ = TUNING.set(tuning);
}

fn current() -> Tuning {
    *TUNING.get().unwrap_or(&Tuning::DEFAULTS)
}

pub(crate) fn task_timeout_secs() -> u64 {
    current().task_timeout_secs
}

pub(crate) fn task_keepalive_secs() -> u64 {
    current().task_keepalive_secs
}

pub(crate) fn task_keepalive_max_secs() -> u64 {
    current().task_keepalive_max_secs
}

pub(crate) fn longpoll_max_ms() -> u64 {
    current().longpoll_max_ms
}
