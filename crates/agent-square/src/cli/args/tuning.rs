//! `TuningOpts` — the hidden per-process knobs, split out of
//! [`SharedServerOpts`](super::shared::SharedServerOpts) so a command can take
//! the tuning without also advertising the daemon flags it does not honor.
//! `discover` is the case in point: it needs the knobs (the suite varies its
//! directory cadence) but writes no state file and runs no gossip session, so
//! flattening the full server group would list `--state-file` / `--max-peers` /
//! `--filter-self` / `--a2a-serve` in its `--help` as silent no-ops.
//!
//! Not in `--help`. Production runs on the `agent_habilis_mesh::util::consts`
//! defaults; the subprocess test suite passes these to run with short timings.
//! These replace the former env-var overrides — see
//! `agent_habilis_mesh::util::tuning`.

use clap::Parser;

use agent_habilis_mesh::util::consts;

#[derive(Parser, Debug)]
pub(crate) struct TuningOpts {
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

    /// First rival re-check shed of an `EagerProbed` public beacon (seconds).
    #[arg(long, hide = true, default_value_t = consts::RIVAL_RECHECK_FIRST_SECS)]
    pub rival_recheck_first_secs: u64,

    /// Steady rival re-check cadence for a lone beacon holder (seconds).
    #[arg(long, hide = true, default_value_t = consts::RIVAL_RECHECK_SECS)]
    pub rival_recheck_secs: u64,

    /// Steady rival re-check cadence while meshed (seconds).
    #[arg(long, hide = true, default_value_t = consts::RIVAL_RECHECK_MESHED_SECS)]
    pub rival_recheck_meshed_secs: u64,

    /// Narrow topic-mesh lookups to mDNS only (no DHT, no relay).
    #[arg(long, hide = true, default_value_t = false)]
    pub topic_mdns_only: bool,

    /// Register the multi-hop transport on the participant endpoint: a directed
    /// message to a peer with no direct path rides the multihop path (relayed
    /// through peers). Stands up a second underlay endpoint.
    #[arg(long, hide = true, default_value_t = false)]
    pub multihop: bool,
}

impl TuningOpts {
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
            rival_recheck_first_secs: self.rival_recheck_first_secs,
            rival_recheck_secs: self.rival_recheck_secs,
            rival_recheck_meshed_secs: self.rival_recheck_meshed_secs,
            topic_mdns_only: self.topic_mdns_only,
        }
    }
}
