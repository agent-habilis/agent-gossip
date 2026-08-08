//! Process tuning for the parts of the A2A layer the engine does not own: the
//! task state machine and the long-poll park.
//!
//! Mirrors `fofoca::util::tuning` in shape — a `const` default per
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

/// How long `a2a call` waits for a directed peer response (seconds), and the
/// `--timeout-secs` default.
///
/// **Must stay above the engine's heal interval**, currently 15s
/// (`fofoca::util::consts::HEAL_INTERVAL_SECS`). A directed request
/// rides the gossip overlay unlogged, so one sent while the overlay holds no
/// live peer link is dropped outright and anti-entropy never heals it; the
/// sender is rescued only when the next heal tick re-bridges the pair. This sat
/// at 15 — exactly one heal interval — which made the rescue arrive at the same
/// moment the caller gave up, so a call issued while the overlay was still
/// converging was a coin flip. It lost every time on a 2-vCPU CI runner.
///
/// The cost of the larger value is that a genuinely unreachable peer takes
/// longer to report as such. That is the right way round: a false failure
/// against a healthy peer is worse than a slow answer about a dead one.
pub(crate) const CALL_TIMEOUT_SECS: u64 = 45;

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

/// Max **live** (non-terminal) tasks one peer may hold with us. A task offer
/// is the one registry entry a remote peer mints directly, so without a quota
/// a single member can open them at line rate. Per-peer rather than global so
/// a flooder exhausts only its own allowance and honest peers keep working.
///
/// Counts non-terminal records only: terminal ones linger for the dedup window
/// (see `task::sweep_registry`), and counting those would let a legitimate
/// burst of short tasks — an orchestrator fanning out to one worker — hit the
/// quota on its own history. A flooder gains nothing, since a freshly minted
/// task is `submitted`, and the only way past the quota is to wait out the
/// idle timeout.
pub(crate) const TASKS_PER_PEER_CAP: usize = 64;

/// Max task records held at once, across every peer — the memory backstop the
/// per-peer quota cannot give on its own, since identities are free (the
/// adversarial suite's sybil tripwire). Counts terminal records too: they
/// occupy the map until the sweep drains them, which is what has to stay
/// bounded.
pub(crate) const TASKS_CAP: usize = 1024;

/// Max characters kept from a peer's `mesh:label`. The label is peer-controlled
/// text a skill splices into a one-line todo subject, so an unbounded one is a
/// display flood rather than a memory one — the brief it rides is already
/// bounded by the frame size.
pub(crate) const TASK_LABEL_MAX_CHARS: usize = 120;

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

#[cfg(test)]
mod tests {
    /// One directed A2A call, three front doors — the CLI's `--timeout-secs`,
    /// the MCP `a2a_call` argument, and the localhost JSON-RPC binding's
    /// directed `SendMessage`. They must agree.
    ///
    /// They did not. The CLI read this constant while MCP defaulted to 15s and
    /// the JSON-RPC path hardcoded 30s. 15s is one heal interval, the value
    /// [`CALL_TIMEOUT_SECS`]'s own comment records as losing "every time" on a
    /// slow runner: the peer minted the task, the caller gave up first, and the
    /// initiator was told a live task had failed.
    ///
    /// The CLI's half of this lives in `cli::args::a2a`, which owns the flag.
    #[test]
    fn mcp_and_the_json_rpc_binding_read_one_call_timeout() {
        assert_eq!(
            crate::mcp::default_a2a_timeout_secs(),
            super::CALL_TIMEOUT_SECS,
            "the MCP a2a_call default drifted from the constant"
        );

        // The JSON-RPC binding has no value to read back: it builds a
        // `Duration` inline. A literal there is what the 30s drift looked
        // like, so the call site itself is the thing to pin.
        let node = include_str!("node.rs");
        assert!(
            node.contains("timeout: Duration::from_secs(crate::a2a::tuning::CALL_TIMEOUT_SECS)"),
            "a2a/node.rs no longer reads the directed-send timeout from this module"
        );
        assert!(
            !node.contains("timeout: Duration::from_secs(3"),
            "a2a/node.rs hardcodes a call timeout again instead of reading the constant"
        );
    }
}
