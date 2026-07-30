//! The single home for constants we may want to tune in the future:
//! runtime paths plus the network-wide wire constants. The few names the
//! external test/bench crates assert against are re-exported from the crate
//! root (see `lib.rs`); the rest stay crate-internal.

// The runtime base for per-mesh files is per-user and computed at runtime —
// see [`crate::util::runtime_base`] / [`crate::util::ensure_runtime_base`].
// It used to be a hardcoded shared `/tmp/agent-gossip` const; that let other
// local users traverse it and read per-member logs, so it moved to a
// uid-scoped, `0700` directory.

/// The mesh sigil — the single definition of the glyph *in code*. It prefixes
/// every mesh id (`💬://<base58>`) and marks every human/operator output line.
/// Display code that wants the emoji-presentation selector appends `\u{FE0F}`
/// itself (see the output sink).
///
/// Re-glyphing is not one line. Help text, doc comments, snapshots and test
/// fixtures spell the glyph literally in ~200 places and cannot interpolate a
/// const, and a stale one is invisible until a user reads the wrong sigil out
/// of `--help` — `mesh-pipe` advertised the previous glyph long after this line
/// moved. Change this, then sweep the literals.
pub const MESH_GLYPH: &str = "💬";

/// The peer sigil — brands a peer's address in its published card interface
/// url (`🤖://<pubkey>`). A robot for the AI agent behind the pubkey; distinct
/// from the mesh `💬` and the ticket `🎟️` so the three token families never
/// collide. Emoji-presentation by default, so no variation selector.
pub const PEER_GLYPH: &str = "🤖";

/// The ticket sigil — brands a bearer capability (`🎟️://<base58check>`),
/// shared by the blob, invite, and application-bridge tickets and told apart by a kind
/// byte in the payload. The base `🎟` (`U+1F39F`) defaults to *text*
/// presentation, so the canonical form carries the `\u{FE0F}` selector that
/// makes it render as the colorful emoji users copy; decoders also accept the
/// bare form.
pub const TICKET_GLYPH: &str = "🎟\u{FE0F}";

/// The `://` that follows every token glyph in its canonical form.
pub const MESH_URI_SEPARATOR: &str = "://";

/// Default lifetime of a minted invite ticket when `--ttl` is omitted (24h).
/// A finite window by default so a forgotten invite stops admitting; the
/// creator can override, and `--ttl none`/`0` mints a no-expiry invite.
pub const INVITE_DEFAULT_TTL_SECS: u64 = 24 * 60 * 60;

/// Max bytes a per-member log file grows before rotating to `<file>.1`
/// (active + one backup ⇒ bounded at `2 ×` this). The `--log-max-bytes` flag
/// overrides; `0` disables rotation. Resolved by
/// [`crate::util::logs::log_max_bytes`].
pub(crate) const LOG_FILE_MAX_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB

/// Maximum size in bytes of a serialized mesh message. A network-wide
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

/// Sanity ceiling on a shard header's advertised `total`. **Not** a memory
/// guard — the reassembly store never allocates by `total` and is bounded by
/// its byte budgets — just a tripwire that rejects an absurd header at
/// `Message::parse`. Sized with ~2x headroom over the most shards a
/// budget-respecting body can need ([`REASSEMBLY_GROUP_MAX_BYTES`] divided by
/// a worst-case per-shard body budget; pinned by a unit test in
/// `protocol::message::shard`).
pub const MAX_SHARD_TOTAL: u32 = 65_536;

/// Largest shard group whose frames still enter the message log (and thus
/// heal via anti-entropy) — exactly the old 16-shard behavior. A bigger
/// group's shards skip the log so one huge body can't evict the mesh's whole
/// anti-entropy history; those transfers are transport-reliable instead
/// (QUIC streams on the unicast connection).
pub const LOGGED_SHARD_GROUP_MAX_TOTAL: u32 = 16;

/// Upper bound on a logical (possibly multipart) body the daemon will accept
/// from a caller — a **local input + surfacing bound, not a wire bound** (the
/// wire is bounded by the reassembly byte budgets below). Enforced by the
/// stdin/IPC readers and the send path; anything larger belongs on the blob
/// channel ([`MAX_BLOB_BYTES`], disk-streamed). Generous, but a bound: the
/// daemon buffers a full input line and every peer's surfaced ring holds full
/// bodies, and a gossip-fallback of one such body already floods
/// `MAX_LOGICAL_BODY_BYTES / MAX_MESSAGE_SIZE` (~18k) frames to every peer.
pub const MAX_LOGICAL_BODY_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// Ceiling applied to an already-**sealed** directed body (task legs, RPC).
/// Sealing base58-encodes the payload (~1.37× inflation), so checking a
/// sealed body against the raw input ceiling would silently shrink the
/// documented limit to ~46 `MiB` for directed sends only; 1.5× headroom keeps
/// the caller-facing ceiling uniform at [`MAX_LOGICAL_BODY_BYTES`].
pub const MAX_SEALED_BODY_BYTES: usize = MAX_LOGICAL_BODY_BYTES + MAX_LOGICAL_BODY_BYTES / 2;

// ── Shard reassembly budgets ──────────────────────────────────────
//
// The reassembly store buffers *partial* multipart bodies until every shard
// arrives. Its bounds are byte budgets (not shard counts), expressed as
// multiples of [`MAX_LOGICAL_BODY_BYTES`] so they scale with the input
// ceiling: sealing (base58) + JSON escaping inflate a body ~1.5×, hence the
// 2× per-group headroom.

/// Ceiling for one partial group's buffered bytes — a sealed max-size body
/// fits; anything claiming more is a crafted stream and the group is dropped.
pub const REASSEMBLY_GROUP_MAX_BYTES: usize = 2 * MAX_LOGICAL_BODY_BYTES;

/// Per-author (pubkey) budget across that author's partial groups. A hostile
/// peer exhausts only its own budget; breaching it evicts that author's
/// stalest incomplete group rather than anyone else's.
pub const REASSEMBLY_AUTHOR_BUDGET_BYTES: usize = 3 * MAX_LOGICAL_BODY_BYTES;

/// Global backstop across all authors — pubkeys are free (Sybil), so the
/// per-author budget alone is not a bound. Breaching it drops the incoming
/// shard's group (fail closed, never balloon).
pub const REASSEMBLY_TOTAL_BUDGET_BYTES: usize = 6 * MAX_LOGICAL_BODY_BYTES;

/// A partial group that gained no **new** shard for this long is reaped
/// (swept on the 1-minute prune tick). Only progress refreshes the clock —
/// a duplicate slot does not, so an attacker can't keepalive a dead group
/// with resends — and a slow-but-live transfer is never reaped mid-flight.
pub(crate) const REASSEMBLY_STALE_SECS: u64 = 300;

/// Hard ceiling on a partial group's lifetime regardless of activity. The
/// idle TTL alone lets a hostile stream pin its buffered bytes forever by
/// trickling one shard per window; this bounds any single group's pin to an
/// hour (generous — a budget-respecting transfer completes in seconds to
/// minutes).
pub(crate) const REASSEMBLY_MAX_GROUP_LIFETIME_SECS: u64 = 3600;

/// Fixed overhead one buffered shard charges on top of its body bytes — the
/// map nodes, cloned keys, and (for shard 0) the retained envelope clone.
/// Charging body bytes alone undercounts a tiny-shard flood by an order of
/// magnitude, letting real memory blow past the budgets the store enforces.
pub(crate) const REASSEMBLY_SLOT_OVERHEAD_BYTES: usize = 512;

/// Minimum total charge for one buffered shard, so a flood of 1-byte crafted
/// shards across millions of groups still pays for the ~KB of real state each
/// one occupies (envelope clone + map/key overhead).
pub(crate) const REASSEMBLY_SLOT_MIN_CHARGE_BYTES: usize = 2048;

/// How long a big (unlogged) partial group may sit without progress before
/// the receiver asks the author to re-send its missing shards (`shard/repair`
/// over the gossip RPC). Checked on the 1-minute prune tick; with the 300s
/// idle TTL this yields several repair rounds before the group is reaped.
pub(crate) const REASSEMBLY_REPAIR_IDLE_SECS: u64 = 60;

/// Max missing indexes one repair request names (and the serve side honors) —
/// bounds both the request body and the resend burst; a wider gap heals over
/// successive rounds.
pub const REASSEMBLY_REPAIR_MAX_IDXS: usize = 64;

/// Byte budget of the sender-side shard cache — the serialized frames of
/// recent big (unlogged) outbound groups, kept so a receiver's
/// `shard/repair` request can be served. Big groups skip the message log
/// (they must not evict the anti-entropy history), so this cache is their
/// only re-serve source. Whole-group FIFO eviction; sized to hold one
/// max-size group.
pub(crate) const SHARD_CACHE_BUDGET_BYTES: usize = REASSEMBLY_GROUP_MAX_BYTES;

/// Largest single file the blob channel will offload. Streamed from disk on
/// both ends (never buffered whole), so this is a disk-bound per-blob ceiling,
/// not a memory cap — a generous limit that stops a hostile ticket from
/// claiming an absurd `size`.
pub(crate) const MAX_BLOB_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// Spool-disk budget for one peer's blob store (`<nick>.blobs/`). Snapshotting a
/// new blob that would push the store past this unlinks the oldest spooled
/// blobs to make room — a hard cap, so it can drop a still-referenced blob under
/// pressure (the fetch then fails cleanly rather than corrupting).
pub(crate) const MAX_BLOB_STORE_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB

/// Default number of recent messages each member retains in its in-memory
/// log (anti-entropy recovery source + poll/fetch history). A fixed value
/// (see `tuning::message_log_size`); edit + commit here to change it. A
/// bigger log lets a reconnecting peer recover a
/// longer gap. Not coupled to the IPC response cap — that is the separate,
/// fixed [`POLL_RESPONSE_MAX_MSGS`] (the log can exceed it; `poll` then
/// surfaces the most-recent window and anti-entropy carries the rest).
pub(crate) const DEFAULT_MESSAGE_LOG_SIZE: usize = 1000;

/// Max messages a single `poll` / `fetch_messages` returns — a **fixed**
/// IPC contract (the `agent-gossip poll` client can't know the daemon's configured
/// log size, so the read cap can't depend on it). At the default log size
/// this equals the log, so `poll` returns everything; a larger configured
/// log just means `poll` surfaces the most-recent `POLL_RESPONSE_MAX_MSGS`.
pub const POLL_RESPONSE_MAX_MSGS: usize = 1000;

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
pub const SURFACED_EVENTS_CAP: usize = POLL_RESPONSE_MAX_MSGS;

/// Byte budget of the surfaced-events ring, alongside its count cap: with
/// multipart bodies up to [`MAX_LOGICAL_BODY_BYTES`], a run of large messages
/// would otherwise hold `SURFACED_EVENTS_CAP × body` in memory. 3× the body
/// ceiling holds at least **two** max-size bodies plus a tail — two ordinary
/// back-to-back sends must never evict the first before a normally-paced
/// poll client reads it; older events oldest-drop exactly like the count cap.
pub const SURFACED_RING_BUDGET_BYTES: usize = 3 * MAX_LOGICAL_BODY_BYTES;

/// Max concurrent parked long-poll waiters per daemon. A blocking `poll` /
/// `fetch_messages` registers a waiter when the buffer is empty; over this cap
/// the read degrades to an immediate (empty) return rather than parking, so the
/// registry can never grow without bound (the bounded-everything discipline).
pub const POLL_WAITERS_CAP: usize = 64;

/// Capacity of the unicast inbound channel — frames the `UNICAST_ALPN` acceptor
/// forwards to the event loop for `gossip::ingest`. Bounded so a peer flooding a
/// unicast stream can't back-pressure the loop; over the cap a frame is dropped
/// (non-blocking `try_send`) and recovered via anti-entropy.
pub(crate) const UNICAST_INBOX_CAP: usize = 256;

/// Max bytes for one IPC command line: a full logical body in a JSON envelope
/// (mesh id, nickname, keys). The body travels JSON-escaped inside the line
/// — worst case every char doubles (quotes/backslashes/newlines) — so budget
/// **2×** the raw ceiling plus envelope headroom, or an escape-heavy body
/// within the documented limit is refused at the socket.
pub(crate) const MAX_IPC_COMMAND_BYTES: usize = 2 * MAX_LOGICAL_BODY_BYTES + 2 * MAX_MESSAGE_SIZE;

/// Max bytes for one IPC response line. A rendered event carries the body
/// **twice** (the raw field plus the `display` re-render), each JSON-escaped
/// (worst case 2× the raw bytes), so one event can render to ~**4×** its raw
/// body plus envelope — the response cap must clear that for a max-size body
/// or the event could never be polled and the client's cursor would wedge on
/// it forever. `poll` batches byte-aware against this bound
/// (`EventLoopState::poll_since`), returning the oldest prefix that fits;
/// the client re-polls for the rest.
pub(crate) const MAX_IPC_RESPONSE_BYTES: usize = 5 * MAX_LOGICAL_BODY_BYTES;

// ── Password KDF (wire contract) ──────────────────────────────────
//
// Argon2id cost parameters for `--password` stretching. A NETWORK-WIDE WIRE
// CONTRACT: every member derives the stretched key with these exact params
// (the derivation feeds the mesh topic/rendezvous and the ticket handshake
// token), so changing any value strands every existing passworded mesh and
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
// const. See `agent_habilis_mesh::util::tuning`.

/// How long a peer can go unheard before the sweeper evicts it. Must exceed
/// the alive-keepalive interval comfortably (3× absorbs one or two lost
/// rounds). Flag: `--alive-timeout-secs` (tests shorten it to seconds).
pub const ALIVE_TIMEOUT_SECS: u64 = 90;

/// How often the sweeper walks `last_seen` looking for expired peers.
/// Flag: `--sweep-interval-secs`.
pub const SWEEP_INTERVAL_SECS: u64 = 10;

/// Idle-debounce timeout for a task: a task with no leg (content
/// or keepalive) for this long is evicted (`task_timeout` + a `Cancel`
/// broadcast). 2 minutes — the timeout doubles as the pickup deadline (a
/// SUBMITTED task whose worker never drives a leg gets no keepalive cover,
/// so it is reaped this long after dispatch) and stays comfortably above
/// the keepalive cadence so a genuinely-active task is never falsely
/// evicted. Flag: `--task-timeout-secs` (tests shorten it to seconds).
pub const TASK_TIMEOUT_SECS: u64 = 120;

/// How often the current ball-owner's daemon emits a `Progress` keepalive
/// for a live task. 30 seconds, ≈4× under the debounce, so ~3 missed beats
/// of slack absorb gossip drops. Flag: `--task-keepalive-secs`.
pub const TASK_KEEPALIVE_SECS: u64 = 30;

/// The longest the ball-owner's daemon keeps auto-covering a **silent** task
/// since its last real leg. The keepalive must not read the same clock the
/// idle-timeout reads, or it feeds the timeout it is subject to and a crashed
/// skill holds the peer forever. So the daemon auto-covers a quiet ball-owner
/// for at most this long; past it, the *skill* must send its own `progress`
/// beat to prove liveness (which refreshes the window), else the keepalive
/// stops and the peer's debounce reaps the task. 2 minutes — the skills'
/// beat contract is one beat per minute of work, so a live worker refreshes
/// the window with margin while a dead one is detected within this window
/// plus one debounce (≈2–4 minutes). Flag: `--task-keepalive-max-secs`.
pub const TASK_KEEPALIVE_MAX_SECS: u64 = 120;

/// Grace before an unmeshed joiner co-hosts the rendezvous anyway (empty
/// mesh ⇒ become the beacon for the next joiner). Flag:
/// `--beacon-cohost-grace-secs`.
pub const BEACON_COHOST_GRACE_SECS: u64 = 10;

// A public `EagerProbed` beacon holder (topic joiner, directory advertiser)
// periodically *sheds* its beacon and re-runs probe-before-claim, because two
// members that claimed inside each other's probe window both hold the same
// `rendezvous_id` and each captures its own bootstrap dial — a split that
// nothing else repairs (see `daemon::event_loop::shed_rival_beacon_if_due`).

/// First shed after a probed public claim. Early, because the likeliest rival
/// is a peer that started within seconds of us — but past the claim's own
/// probe + relay-home warmup, so a genuinely lone holder isn't churned while
/// still settling. Flag: `--rival-recheck-first-secs`.
pub const RIVAL_RECHECK_FIRST_SECS: u64 = 12;

/// Steady shed cadence while the holder has no real-peer links. A lone
/// holder's shed disturbs nobody, so it re-checks briskly — this cadence
/// bounds how long two mutually-invisible lone holders stay split. Flag:
/// `--rival-recheck-secs`.
pub const RIVAL_RECHECK_SECS: u64 = 30;

/// Steady shed cadence while meshed — the island-vs-island backstop. Slow,
/// because a healthy gossip pays the shed's ~probe-budget beacon blip each
/// cycle and a meshed split (two multi-member islands) is already rare.
/// Flag: `--rival-recheck-meshed-secs`.
pub const RIVAL_RECHECK_MESHED_SECS: u64 = 300;

/// Span of the deterministic per-node phase offset added to the first shed,
/// derived from the peer endpoint id. Orders simultaneous claimants so
/// the earlier-offset node sheds first, finds the other's still-held beacon,
/// and yields — a tie-break, not a delay knob, so no flag.
pub const RIVAL_RECHECK_OFFSET_SPAN_SECS: u64 = 8;

/// How long an `agent-gossip ping` round collects pongs before the daemon emits its
/// `ping_report`. Flag: `--ping-window-secs`.
pub const PING_WINDOW_SECS: u64 = 10;

/// Cadence of the unconditional gossip healer (`gossip::heal::tick_heal`).
/// 15s balances fast re-mesh after a partition against steady-state cost —
/// one HyParView control message per tick when already healthy; shorter
/// cadences empirically destabilise convergence in production meshes. Flag:
/// `--heal-interval-secs` (tests shorten it to collapse the multi-cycle
/// rendezvous-handoff floor; production always runs the default).
pub const HEAL_INTERVAL_SECS: u64 = 15;

/// A heal inter-tick gap above this (seconds) means the process was frozen
/// between ticks (App Nap / sleep) and must hard re-bootstrap. Safely above
/// the default heal interval so normal slack never trips it; a test that
/// shortens one must keep this comfortably above `--heal-interval-secs`.
/// Flag: `--heal-stall-threshold-secs`.
pub const HEAL_STALL_THRESHOLD_SECS: u64 = 60;

/// No verified inbound gossip for this long (seconds), while real peers are
/// known, trips the starvation watchdog (re-bridge + re-announce). 2× the
/// alive timeout: by then the sweeper has evicted the whole roster, and an
/// idle-but-healthy mesh stays far inside it (keepalives every 30s).
/// Deliberately its own knob, NOT derived from `--alive-timeout-secs` — the
/// short-evict test profile must not arm the watchdog in unrelated tests.
/// Flag: `--starvation-threshold-secs`.
pub const STARVATION_THRESHOLD_SECS: u64 = 2 * ALIVE_TIMEOUT_SECS;

/// How often an advertising `create` re-broadcasts its `💬…` id into the
/// directory. Flag: `--advertise-interval-secs`.
pub const ADVERTISE_INTERVAL_SECS: u64 = 20;

/// How long a discoverer keeps showing a mesh after its last ad (~3×
/// `ADVERTISE_INTERVAL_SECS`). Flag: `--directory-expiry-secs`.
pub const DIRECTORY_EXPIRY_SECS: u64 = 60;

/// How often a member broadcasts its anti-entropy digest (recent message
/// ids it holds) so peers re-send anything it missed while
/// partitioned/asleep. Flag: `--antientropy-interval-secs` (tests
/// reconcile backfill gaps in seconds).
pub const ANTIENTROPY_INTERVAL_SECS: u64 = 10;

/// Max messages re-broadcast in response to one received digest, so a
/// far-behind peer can't trigger an unbounded backfill burst. Flag:
/// `--antientropy-max-resend`.
pub const ANTIENTROPY_MAX_RESEND: usize = 64;

/// How long the daemon parks a `long: true` `poll` / `fetch_messages` read
/// before returning an empty batch — the single long-poll park length. Kept
/// under typical MCP-host per-request timeouts so a held call returns before
/// the host gives up; the daemon itself never blocks (the waiter parks in a
/// registry). Infinite waiting is a *client* concern: `agent-gossip poll --long`
/// re-issues the read on each empty return. Flag: `--longpoll-max-ms`
/// (tests shorten it to force the timeout path).
pub const LONGPOLL_MAX_MS: u64 = 60_000;

/// How often the CLI daemon checks whether its spawning agent is still alive
/// by re-reading its parent pid. When the parent dies (hard-kill / reinstall),
/// the daemon is orphaned and reparents away; on the next check it self-quits
/// through the normal `left`-broadcasting shutdown so it never lingers in the
/// mesh. Sub-second precision isn't needed — a second or two of orphan
/// lifetime is fine. Flag: `--ppid-watch-interval-ms` (tests shorten it).
pub const PPID_WATCH_INTERVAL_MS: u64 = 1500;

/// Soft resident-memory threshold (`MiB`) above which the daemon emits a
/// one-shot `warn` on its slow prune tick — the in-process leak-visibility
/// signal. (Resident memory = the physical RAM the process holds; the resident
/// set size, RSS.) Warn-only; `0` disables. A pure const (no flag): an operator
/// tunes it by editing here.
pub(crate) const RESIDENT_MEMORY_WARN_MB: u64 = 1024;

/// HyParView **active view** capacity — the number of direct gossip neighbors
/// (open QUIC links) each member maintains per topic. A mesh at or below this
/// size forms a **full mesh** with nothing to shuffle, so it has **zero
/// membership churn** (and thus none of the per-connection-churn memory leak);
/// past it the overlay maintains a partial mesh and continuously
/// promotes/demotes peers (the churn). Raised from iroh-gossip's default of 5
/// to **64** so realistic agent meshes (≤ 65) stay churn-free. The ceiling is
/// performance, not correctness: each slot is a live connection + keepalive
/// (~0.5 MB resident per link) and a full mesh costs O(S²) broadcast
/// amplification, so a fully-meshed node runs ~50 MB — 64 deliberately trades
/// that heavier per-node cost for a larger churn-free mesh. This is the default
/// for the public `--max-peers` cap; the passive (healing/shuffle) pool is
/// derived as 2× the live view. Set `--max-peers` *small* to deliberately
/// reproduce the gossip-churn leak at any node count.
pub const GOSSIP_ACTIVE_VIEW_CAPACITY: usize = 64;

// QUIC keep-alive / idle timeout are intentionally left at iroh's
// holepunch-tuned transport defaults (~1s keep-alive, 15s direct / 30s relay
// idle); see `lookup::build_endpoint`. A prior override (5s keep-alive, 10s
// idle) fought iroh's QUIC-multipath tuning and drove connection-churn — a
// per-connection memory leak. The sleep-wake reliability tests therefore freeze
// a peer past iroh's 15s direct-path idle to force a link death.
