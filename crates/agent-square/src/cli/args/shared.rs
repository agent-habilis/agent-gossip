//! `SharedServerOpts` — the option group flattened into every
//! long-running server command (`create`, `join`, `discover`). Holds
//! only genuinely-local, per-process settings; lookup selection is a
//! square-wide property carried in the id (see `create`'s `LookupArgs`).

use clap::Parser;

use agent_habilis_mesh::util::consts;

/// Shared options for server commands.
#[derive(Parser, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "flat clap flag group; each bool is an independent CLI switch, not a state machine to model as an enum"
)]
pub(crate) struct SharedServerOpts {
    /// Suppress self-authored messages from stdout (for Monitor use)
    #[arg(long, default_value_t = false)]
    pub filter_self: bool,

    /// Cap on live direct connections (the gossip overlay's active-neighbor
    /// set). The square holds at most this many QUIC links and relays to peers
    /// beyond it; squares up to this size form a full mesh with no membership
    /// churn.
    #[arg(long, default_value_t = consts::GOSSIP_ACTIVE_VIEW_CAPACITY)]
    pub max_peers: usize,

    /// Serve the A2A JSON-RPC 2.0 binding on 127.0.0.1 (off by default).
    ///
    /// Optional value = the TCP port; omit it (or pass 0) for an
    /// OS-assigned port. The bound port and the per-daemon bearer token are
    /// written to the session state file and the `ready` event
    /// (`a2a_port`), so a local A2A client can discover both. The card is
    /// served unauthenticated at `/.well-known/agent-card.json`; every
    /// JSON-RPC call requires `Authorization: Bearer <token>`.
    #[arg(long = "a2a-serve", num_args = 0..=1, default_missing_value = "0")]
    pub a2a_serve: Option<u16>,

    /// Override the session state-file path. The daemon writes
    /// `{square, name, nickname, participant_count, ready, last_updated}` to a
    /// JSON file on every peer-set change and a ~10s heartbeat, and deletes it
    /// on clean shutdown — for external tools (e.g. a shell statusline) to
    /// render live count + liveness without IPC. Defaults to
    /// `<runtime-base>/<mesh-prefix>/<nick>.state.json` (beside the socket +
    /// log); pass this to write elsewhere instead.
    #[arg(long)]
    pub state_file: Option<std::path::PathBuf>,

    // ── Hidden tuning knobs ───────────────────────────────────────
    // Not in `--help`. Production runs on the `agent_habilis_mesh::util::consts`
    // defaults below; the subprocess test suite passes these to run with
    // short timings. These replace the former env-var overrides — see
    // `agent_habilis_mesh::util::tuning`.
    /// Peer-eviction silence timeout (seconds).
    #[arg(long, hide = true, default_value_t = consts::ALIVE_TIMEOUT_SECS)]
    pub alive_timeout_secs: u64,

    /// How often the sweeper scans for expired peers (seconds).
    #[arg(long, hide = true, default_value_t = consts::SWEEP_INTERVAL_SECS)]
    pub sweep_interval_secs: u64,

    /// Cadence of the unconditional gossip healer (seconds).
    #[arg(long, hide = true, default_value_t = consts::HEAL_INTERVAL_SECS)]
    pub heal_interval_secs: u64,

    /// Task idle-debounce timeout (seconds).
    #[arg(long, hide = true, default_value_t = consts::TASK_TIMEOUT_SECS)]
    pub task_timeout_secs: u64,

    /// Task keepalive cadence for the ball-owner (seconds).
    #[arg(long, hide = true, default_value_t = consts::TASK_KEEPALIVE_SECS)]
    pub task_keepalive_secs: u64,

    /// Longest the daemon auto-covers a silent task without a skill leg (seconds).
    #[arg(long, hide = true, default_value_t = consts::TASK_KEEPALIVE_MAX_SECS)]
    pub task_keepalive_max_secs: u64,

    /// Grace before an unmeshed joiner co-hosts the rendezvous (seconds).
    #[arg(long, hide = true, default_value_t = consts::BEACON_COHOST_GRACE_SECS)]
    pub beacon_cohost_grace_secs: u64,

    /// How long an `agent-square ping` round collects pongs (seconds).
    #[arg(long, hide = true, default_value_t = consts::PING_WINDOW_SECS)]
    pub ping_window_secs: u64,

    /// How often the daemon checks for orphaning by its spawning agent (millis).
    #[arg(long, hide = true, default_value_t = consts::PPID_WATCH_INTERVAL_MS)]
    pub ppid_watch_interval_ms: u64,

    /// How long a `long: true` poll read parks before returning empty (millis).
    #[arg(long, hide = true, default_value_t = consts::LONGPOLL_MAX_MS)]
    pub longpoll_max_ms: u64,

    /// Heal inter-tick gap above which the process hard re-bootstraps (seconds).
    #[arg(long, hide = true, default_value_t = consts::HEAL_STALL_THRESHOLD_SECS)]
    pub heal_stall_threshold_secs: u64,

    /// No inbound gossip for this long, with peers known, trips the starvation watchdog (seconds).
    #[arg(long, hide = true, default_value_t = consts::STARVATION_THRESHOLD_SECS)]
    pub starvation_threshold_secs: u64,

    /// Directory re-broadcast cadence for an advertiser (seconds).
    #[arg(long, hide = true, default_value_t = consts::ADVERTISE_INTERVAL_SECS)]
    pub advertise_interval_secs: u64,

    /// How long a discoverer keeps showing a square after its last ad (seconds).
    #[arg(long, hide = true, default_value_t = consts::DIRECTORY_EXPIRY_SECS)]
    pub directory_expiry_secs: u64,

    /// How often a member broadcasts its anti-entropy digest (seconds).
    #[arg(long, hide = true, default_value_t = consts::ANTIENTROPY_INTERVAL_SECS)]
    pub antientropy_interval_secs: u64,

    /// Max messages re-sent in response to one anti-entropy digest.
    #[arg(long, hide = true, default_value_t = consts::ANTIENTROPY_MAX_RESEND)]
    pub antientropy_max_resend: usize,

    /// Use the loopback (private) directory + relax the advertise→public guard.
    #[arg(long, hide = true, default_value_t = false)]
    pub directory_private: bool,

    /// Disable the unicast (point-to-point) transport: force every message onto
    /// gossip, the pre-unicast behavior.
    #[arg(long, hide = true, default_value_t = false)]
    pub no_unicast: bool,

    /// Make directed messages unicast-only: gossip no longer carries or falls
    /// back for them (broadcasts still ride gossip). Tests use this to prove
    /// unicast delivery in isolation.
    #[arg(long, hide = true, default_value_t = false)]
    pub no_gossip_directed: bool,

    /// Register the multi-hop transport on the participant endpoint: a directed
    /// message to a peer with no direct path rides the multihop path (relayed
    /// through peers) instead of gossip. Stands up a second underlay endpoint.
    #[arg(long, hide = true, default_value_t = false)]
    pub multihop: bool,
}

impl SharedServerOpts {
    /// The process tuning carried by these flags, for [`agent_habilis_mesh::util::tuning::init`].
    pub(crate) fn tuning(&self) -> agent_habilis_mesh::util::tuning::Tuning {
        agent_habilis_mesh::util::tuning::Tuning {
            alive_timeout_secs: self.alive_timeout_secs,
            sweep_interval_secs: self.sweep_interval_secs,
            heal_interval_secs: self.heal_interval_secs,
            task_timeout_secs: self.task_timeout_secs,
            task_keepalive_secs: self.task_keepalive_secs,
            task_keepalive_max_secs: self.task_keepalive_max_secs,
            cohost_grace_secs: self.beacon_cohost_grace_secs,
            ping_window_secs: self.ping_window_secs,
            ppid_watch_interval_ms: self.ppid_watch_interval_ms,
            longpoll_max_ms: self.longpoll_max_ms,
            heal_stall_threshold_secs: self.heal_stall_threshold_secs,
            starvation_threshold_secs: self.starvation_threshold_secs,
            advertise_interval_secs: self.advertise_interval_secs,
            directory_expiry_secs: self.directory_expiry_secs,
            antientropy_interval_secs: self.antientropy_interval_secs,
            antientropy_max_resend: self.antientropy_max_resend,
            directory_private: self.directory_private,
        }
    }

    /// The per-session transport policy these flags select. Threaded into the
    /// session config (not the process-global tuning) so it stays a session
    /// property. See [`agent_habilis_mesh::transport::TransportPolicy`].
    pub(crate) fn transport_policy(&self) -> agent_habilis_mesh::transport::TransportPolicy {
        agent_habilis_mesh::transport::TransportPolicy {
            unicast: !self.no_unicast,
            gossip_directed: !self.no_gossip_directed,
        }
    }
}
