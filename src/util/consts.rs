//! The single home for constants we may want to tune in the future:
//! runtime paths plus the network-wide wire constants. The few names the
//! external test/bench crates assert against are re-exported from the crate
//! root (see `lib.rs`); the rest stay crate-internal.

/// Runtime base for all per-swarm files. Each swarm gets a folder
/// `<RUNTIME_DIR>/<swarm-prefix>/` holding its members' `<nick>.tracing.log`,
/// `<nick>.ipc.sock`, and `<nick>.state.json`. Hardcoded `/tmp` base — short
/// (avoids the macOS `AF_UNIX` `sun_path` ~104-byte limit) and shared with
/// sibling agent-habilis projects in the `/tmp/agent-habilis/` namespace.
/// Per-swarm paths are built via [`crate::util::swarm_runtime_dir`].
pub const RUNTIME_DIR: &str = "/tmp/agent-habilis/swarm";

/// Max bytes a per-member log file grows before rotating to `<file>.1`
/// (active + one backup ⇒ bounded at `2 ×` this). The `--log-max-bytes` flag
/// overrides; `0` disables rotation. Resolved by
/// [`crate::util::logs::log_max_bytes`].
pub(crate) const LOG_FILE_MAX_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB

/// Maximum size in bytes of a serialized swarm message. A network-wide
/// wire contract (must be uniform across members), so it lives here.
///
/// Kept below iroh-gossip's `DEFAULT_MAX_MESSAGE_SIZE` (4096) minus its
/// ~39-byte wire header: a message larger than gossip's payload budget
/// is silently dropped by the gossip layer (it never propagates and the
/// sender gets no error), so our cap must stay under it. A compile-time
/// assertion in the binary guards that relationship against the live
/// gossip constant; the value is hardcoded here rather than derived from
/// iroh-gossip's (so this module pulls in no dependency).
pub const MAX_MESSAGE_SIZE: usize = 3840;

/// Maximum number of parts a single logical body is split into when it exceeds
/// [`MAX_MESSAGE_SIZE`]. Each part is an ordinary message that occupies one
/// message-log slot, so this also bounds how many slots one body consumes and
/// caps a crafted peer's reassembly buffering. A body that would need more
/// parts than this is refused on send.
pub const MAX_MESSAGE_PARTS: usize = 16;

/// Upper bound on a logical (possibly multipart) body the daemon will accept
/// from a caller — the input ceiling for `msg`/`task`. The send path is the
/// real gate (it refuses a body that needs more than [`MAX_MESSAGE_PARTS`]
/// parts); this is the generous limit the stdin/IPC readers enforce so an
/// oversize body still reaches the daemon and gets a clear "too large" error
/// rather than a truncated read.
pub const MAX_LOGICAL_BODY_BYTES: usize = MAX_MESSAGE_PARTS * MAX_MESSAGE_SIZE;

/// Default number of recent messages each member retains in its in-memory
/// log (anti-entropy recovery source + poll/fetch history). A fixed value
/// (see `tuning::message_log_size`); edit + commit here to change it. A
/// bigger log lets a reconnecting peer recover a
/// longer gap. Not coupled to the IPC response cap — that is the separate,
/// fixed [`POLL_RESPONSE_MAX_MSGS`] (the log can exceed it; `poll` then
/// surfaces the most-recent window and anti-entropy carries the rest).
pub(crate) const DEFAULT_MESSAGE_LOG_SIZE: usize = 1000;

/// Max messages a single `poll` / `fetch_messages` returns — a **fixed**
/// IPC contract (the `ahsw poll` client can't know the daemon's configured
/// log size, so the read cap can't depend on it). At the default log size
/// this equals the log, so `poll` returns everything; a larger configured
/// log just means `poll` surfaces the most-recent `POLL_RESPONSE_MAX_MSGS`.
pub(crate) const POLL_RESPONSE_MAX_MSGS: usize = 1000;

/// Capacity of the daemon-local **surfaced-events** ring — the seq-ordered
/// history `poll` / `fetch_messages` drain. Distinct from the message log:
/// that is the cross-node anti-entropy buffer (evicted by a deterministic
/// `eviction_key`); this is a *local* record of what was surfaced to the
/// operator/agent (msg + presence + task legs **and** the transient
/// events — `ping_report`, `peer_timeout`/`return`, `task_timeout`,
/// `fork` — that never entered the message log). Oldest-drop on overflow.
///
/// Sized **equal to** the poll response window so the ring never holds more
/// than a single `poll` returns: ring eviction and the response cap coincide,
/// so the response cap is a no-op in the steady state and no surfaced event is
/// ever dropped *after* `since()` selected it but *before* the client could
/// read it (a larger ring would silently lose the oldest events on a full
/// first poll, with the client's cursor advancing past them).
pub(crate) const SURFACED_EVENTS_CAP: usize = POLL_RESPONSE_MAX_MSGS;

/// Max concurrent parked long-poll waiters per daemon. A blocking `poll` /
/// `fetch_messages` registers a waiter when the buffer is empty; over this cap
/// the read degrades to an immediate (empty) return rather than parking, so the
/// registry can never grow without bound (the bounded-everything discipline).
pub(crate) const POLL_WAITERS_CAP: usize = 64;

/// Max bytes for one stdin line. A body up to a full logical (multipart) body
/// is accepted; the daemon splits it across the wire.
pub(crate) const MAX_STDIN_LINE_BYTES: usize = MAX_LOGICAL_BODY_BYTES;

/// Max bytes for one IPC command line: a full logical body in a JSON envelope
/// (swarm id, nickname, keys). The daemon splits the body across the wire, so
/// the command carries the whole thing; budget envelope headroom on top.
pub(crate) const MAX_IPC_COMMAND_BYTES: usize = MAX_LOGICAL_BODY_BYTES + 2 * MAX_MESSAGE_SIZE;

/// Max bytes for one IPC response line: a poll returns at most
/// [`POLL_RESPONSE_MAX_MSGS`] events. Each rendered event is larger than its
/// raw body — the `display` field re-renders the body with a markdown prefix
/// (≈ a second copy of the body) and the JSON envelope (`seq`/`event`/`id`/
/// `pubkey`/… field names + a `ping_report` peer table) adds more — so budget
/// **3×** the wire cap per event, not 1×, or a window of large-body messages
/// overruns this bound and the `poll` client errors ("IPC response too large").
pub(crate) const MAX_IPC_RESPONSE_BYTES: usize = 3 * POLL_RESPONSE_MAX_MSGS * MAX_MESSAGE_SIZE;

// ── Password KDF (wire contract) ──────────────────────────────────
//
// Argon2id cost parameters for `--password` stretching. A NETWORK-WIDE WIRE
// CONTRACT: every member derives the stretched key with these exact params
// (the derivation feeds the swarm topic/rendezvous and the ticket handshake
// token), so changing any value strands every existing passworded swarm and
// ticket. 19 MiB / t=2 / p=1 is the OWASP Argon2id recommendation — ~50-100ms
// per stretch, paid once at create/join/handshake, never per message.

/// Argon2id memory cost in `KiB` (19 `MiB`).
pub(crate) const PASSWORD_KDF_M_COST_KIB: u32 = 19_456;

/// Argon2id iteration count.
pub(crate) const PASSWORD_KDF_T_COST: u32 = 2;

/// Argon2id lane count.
pub(crate) const PASSWORD_KDF_P_COST: u32 = 1;

// ── Daemon tuning defaults ────────────────────────────────────────
//
// Behavioural knobs that used to be environment-overridable. They now live
// here as constants: an experiment is an *edit + commit* (under version
// control, with history), never an ephemeral shell var. Each is the default
// for the matching hidden CLI flag (`--alive-timeout-secs`, …) that the
// subprocess test suite passes to run with short timings; production reads the
// const. See `agent_habilis_swarm::util::tuning`.

/// How long a peer can go unheard before the sweeper evicts it. Must exceed
/// the alive-keepalive interval comfortably (3× absorbs one or two lost
/// rounds). Flag: `--alive-timeout-secs` (tests shorten it to seconds).
pub(crate) const ALIVE_TIMEOUT_SECS: u64 = 90;

/// How often the sweeper walks `last_seen` looking for expired peers.
/// Flag: `--sweep-interval-secs`.
pub(crate) const SWEEP_INTERVAL_SECS: u64 = 10;

/// Idle-debounce timeout for a task: a task with no leg (content
/// or keepalive) for this long is evicted (`task_timeout` + a `Cancel`
/// broadcast). 5 minutes — comfortably above the keepalive cadence so a
/// genuinely-active task is never falsely evicted. Flag:
/// `--task-timeout-secs` (tests shorten it to seconds).
pub(crate) const TASK_TIMEOUT_SECS: u64 = 300;

/// How often the current ball-owner's daemon emits a `Progress` keepalive
/// for a live task. 1 minute, ≈5× under the debounce, so ~4 missed beats
/// of slack absorb gossip drops. Flag: `--task-keepalive-secs`.
pub(crate) const TASK_KEEPALIVE_SECS: u64 = 60;

/// The longest the ball-owner's daemon keeps auto-covering a **silent** task
/// since its last real leg. The keepalive must not read the same clock the
/// idle-timeout reads, or it feeds the timeout it is subject to and a crashed
/// skill holds the peer forever. So the daemon auto-covers a quiet ball-owner
/// for at most this long; past it, the *skill* must send its own `progress`
/// beat to prove liveness (which refreshes the window), else the keepalive
/// stops and the peer's debounce reaps the task. 15 minutes — comfortably
/// above any human/agent think gap, so a live task doing long work only needs
/// an occasional beat, while a dead one is bounded. Flag:
/// `--task-keepalive-max-secs`.
pub(crate) const TASK_KEEPALIVE_MAX_SECS: u64 = 900;

/// Whole-task budget of **content** legs (offer/accept/decline/context/
/// done/confirm/change/cancel — `progress` is exempt). Hitting it forces
/// the skill to a terminal decision; the daemon warns once on crossing.
pub(crate) const TASK_CONTENT_CAP: u32 = 100;

/// Grace before an unmeshed joiner co-hosts the rendezvous anyway (empty
/// swarm ⇒ become the beacon for the next joiner). Flag:
/// `--beacon-cohost-grace-secs`.
pub(crate) const BEACON_COHOST_GRACE_SECS: u64 = 10;

/// How long an `ahsw ping` round collects pongs before the daemon emits its
/// `ping_report`. Flag: `--ping-window-secs`.
pub(crate) const PING_WINDOW_SECS: u64 = 10;

/// A heal inter-tick gap above this (seconds) means the process was frozen
/// between ticks (App Nap / sleep) and must hard re-bootstrap. Safely above
/// the 15s heal interval so normal slack never trips it. Flag:
/// `--heal-stall-threshold-secs`.
pub(crate) const HEAL_STALL_THRESHOLD_SECS: u64 = 60;

/// No verified inbound gossip for this long (seconds), while real peers are
/// known, trips the starvation watchdog (re-bridge + re-announce). 2× the
/// alive timeout: by then the sweeper has evicted the whole roster, and an
/// idle-but-healthy mesh stays far inside it (keepalives every 30s).
/// Deliberately its own knob, NOT derived from `--alive-timeout-secs` — the
/// short-evict test profile must not arm the watchdog in unrelated tests.
/// Flag: `--starvation-threshold-secs`.
pub(crate) const STARVATION_THRESHOLD_SECS: u64 = 2 * ALIVE_TIMEOUT_SECS;

/// How often an advertising `create` re-broadcasts its `🐝…` id into the
/// directory. Flag: `--advertise-interval-secs`.
pub(crate) const ADVERTISE_INTERVAL_SECS: u64 = 20;

/// How long a discoverer keeps showing a swarm after its last ad (~3×
/// `ADVERTISE_INTERVAL_SECS`). Flag: `--directory-expiry-secs`.
pub(crate) const DIRECTORY_EXPIRY_SECS: u64 = 60;

/// Max messages re-broadcast in response to one received digest, so a
/// far-behind peer can't trigger an unbounded backfill burst. Flag:
/// `--antientropy-max-resend`.
pub(crate) const ANTIENTROPY_MAX_RESEND: usize = 64;

/// How long the daemon parks a `long: true` `poll` / `fetch_messages` read
/// before returning an empty batch — the single long-poll park length. Kept
/// under typical MCP-host per-request timeouts so a held call returns before
/// the host gives up; the daemon itself never blocks (the waiter parks in a
/// registry). Infinite waiting is a *client* concern: `ahsw poll --long`
/// re-issues the read on each empty return. Flag: `--longpoll-max-ms`
/// (tests shorten it to force the timeout path).
pub(crate) const LONGPOLL_MAX_MS: u64 = 60_000;

/// How often the CLI daemon checks whether its spawning agent is still alive
/// by re-reading its parent pid. When the parent dies (hard-kill / reinstall),
/// the daemon is orphaned and reparents away; on the next check it self-quits
/// through the normal `left`-broadcasting shutdown so it never lingers in the
/// swarm. Sub-second precision isn't needed — a second or two of orphan
/// lifetime is fine. Flag: `--ppid-watch-interval-ms` (tests shorten it).
pub(crate) const PPID_WATCH_INTERVAL_MS: u64 = 1500;

/// Soft resident-memory threshold (`MiB`) above which the daemon emits a
/// one-shot `warn` on its slow prune tick — the in-process leak-visibility
/// signal. (Resident memory = the physical RAM the process holds; the resident
/// set size, RSS.) Warn-only; `0` disables. A pure const (no flag): an operator
/// tunes it by editing here.
pub(crate) const RESIDENT_MEMORY_WARN_MB: u64 = 1024;

/// HyParView **active view** capacity — the number of direct gossip neighbors
/// (open QUIC links) each member maintains per topic. A swarm at or below this
/// size forms a **full mesh** with nothing to shuffle, so it has **zero
/// membership churn** (and thus none of the per-connection-churn memory leak);
/// past it the overlay maintains a partial mesh and continuously
/// promotes/demotes peers (the churn). Raised from iroh-gossip's default of 5
/// to **32** so realistic agent swarms (≤ 33) stay churn-free. The ceiling is
/// performance, not correctness: each slot is a live connection + keepalive,
/// and a full mesh costs O(S²) broadcast amplification, so ~48–50 is the
/// practical upper bound on Pi-class hardware. Distinct from the
/// `DEFAULT_MAX_DIRECT_PEERS` (25) soft address-tracking cap. Flag (hidden):
/// `--active-view-capacity` — set it *small* to deliberately reproduce the
/// gossip-churn leak at any node count.
pub(crate) const GOSSIP_ACTIVE_VIEW_CAPACITY: usize = 32;

/// HyParView **passive view** capacity — the backup contact pool used for
/// healing/shuffle when active-view links drop. Kept ≥ 2× the active view
/// (iroh-gossip default 30). Flag (hidden): `--passive-view-capacity`.
pub(crate) const GOSSIP_PASSIVE_VIEW_CAPACITY: usize = 64;

// QUIC keep-alive / idle timeout are intentionally left at iroh's
// holepunch-tuned transport defaults (~1s keep-alive, 15s direct / 30s relay
// idle); see `lookup::build_endpoint`. A prior override (5s keep-alive, 10s
// idle) fought iroh's QUIC-multipath tuning and drove connection-churn — a
// per-connection memory leak. The sleep-wake reliability tests therefore freeze
// a peer past iroh's 15s direct-path idle to force a link death.
