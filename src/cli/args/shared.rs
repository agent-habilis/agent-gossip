//! `SharedServerOpts` — the option group flattened into every
//! long-running server command (`create`, `join`, `discover`). Holds
//! only genuinely-local, per-process settings; lookup selection is a
//! swarm-wide property carried in the id (see `create`'s `LookupArgs`).

use clap::Parser;

use crate::util::consts;

use crate::util::tuning::DEFAULT_MAX_DIRECT_PEERS;

use super::output::OutputFormat;

/// Shared options for server commands.
#[derive(Parser, Debug)]
pub(crate) struct SharedServerOpts {
    /// Disable interactive message input from stdin
    #[arg(long, default_value_t = false)]
    pub no_interactive: bool,

    /// Output format: human (default) or json (structured JSON lines)
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,

    /// Suppress self-authored messages from stdout (for Monitor use)
    #[arg(long, default_value_t = false)]
    pub filter_self: bool,

    /// Soft ceiling on tracked peer addresses (gossip relays beyond
    /// this). Note: the gossip overlay maintains HyParView's
    /// `active_view_capacity` (5) active neighbors regardless — this is
    /// not the live connection count.
    #[arg(long, default_value_t = DEFAULT_MAX_DIRECT_PEERS)]
    pub max_peers: usize,

    /// Override the session state-file path. The daemon writes
    /// `{swarm, name, nickname, participant_count, ready, last_updated}` to a
    /// JSON file on every peer-set change and a ~10s heartbeat, and deletes it
    /// on clean shutdown — for external tools (e.g. a shell statusline) to
    /// render live count + liveness without IPC. Defaults to
    /// `<RUNTIME_DIR>/<swarm-prefix>/<nick>.state.json` (beside the socket +
    /// log); pass this to write elsewhere instead.
    #[arg(long)]
    pub state_file: Option<std::path::PathBuf>,

    // ── Hidden tuning knobs ───────────────────────────────────────
    // Not in `--help`. Production runs on the `crate::util::consts`
    // defaults below; the subprocess test suite passes these to run with
    // short timings. These replace the former env-var overrides — see
    // `crate::util::tuning`.
    /// Peer-eviction silence timeout (seconds).
    #[arg(long, hide = true, default_value_t = consts::ALIVE_TIMEOUT_SECS)]
    pub alive_timeout_secs: u64,

    /// How often the sweeper scans for expired peers (seconds).
    #[arg(long, hide = true, default_value_t = consts::SWEEP_INTERVAL_SECS)]
    pub sweep_interval_secs: u64,

    /// Task idle-debounce timeout (seconds).
    #[arg(long, hide = true, default_value_t = consts::TASK_TIMEOUT_SECS)]
    pub task_timeout_secs: u64,

    /// Task keepalive cadence for the ball-owner (seconds).
    #[arg(long, hide = true, default_value_t = consts::TASK_KEEPALIVE_SECS)]
    pub task_keepalive_secs: u64,

    /// Grace before an unmeshed joiner co-hosts the rendezvous (seconds).
    #[arg(long, hide = true, default_value_t = consts::BEACON_COHOST_GRACE_SECS)]
    pub beacon_cohost_grace_secs: u64,

    /// How long an `ahsw ping` round collects pongs (seconds).
    #[arg(long, hide = true, default_value_t = consts::PING_WINDOW_SECS)]
    pub ping_window_secs: u64,

    /// How often the daemon checks for orphaning by its spawning agent (millis).
    #[arg(long, hide = true, default_value_t = consts::PPID_WATCH_INTERVAL_MS)]
    pub ppid_watch_interval_ms: u64,

    /// Heal inter-tick gap above which the process hard re-bootstraps (seconds).
    #[arg(long, hide = true, default_value_t = consts::HEAL_STALL_THRESHOLD_SECS)]
    pub heal_stall_threshold_secs: u64,

    /// No inbound gossip for this long, with peers known, trips the starvation watchdog (seconds).
    #[arg(long, hide = true, default_value_t = consts::STARVATION_THRESHOLD_SECS)]
    pub starvation_threshold_secs: u64,

    /// Directory re-broadcast cadence for an advertiser (seconds).
    #[arg(long, hide = true, default_value_t = consts::ADVERTISE_INTERVAL_SECS)]
    pub advertise_interval_secs: u64,

    /// How long a discoverer keeps showing a swarm after its last ad (seconds).
    #[arg(long, hide = true, default_value_t = consts::DIRECTORY_EXPIRY_SECS)]
    pub directory_expiry_secs: u64,

    /// Max messages re-sent in response to one anti-entropy digest.
    #[arg(long, hide = true, default_value_t = consts::ANTIENTROPY_MAX_RESEND)]
    pub antientropy_max_resend: usize,

    /// Use the loopback (private) directory + relax the advertise→public guard.
    #[arg(long, hide = true, default_value_t = false)]
    pub directory_private: bool,

    /// HyParView active-view capacity (direct gossip neighbors). Set *small*
    /// (e.g. 5) to deliberately reproduce the gossip partial-mesh churn / leak.
    #[arg(long, hide = true, default_value_t = consts::GOSSIP_ACTIVE_VIEW_CAPACITY)]
    pub active_view_capacity: usize,

    /// HyParView passive-view capacity (healing/shuffle contact pool).
    #[arg(long, hide = true, default_value_t = consts::GOSSIP_PASSIVE_VIEW_CAPACITY)]
    pub passive_view_capacity: usize,
}

/// The hidden directory-tuning knobs for the ticket-advertising transfer
/// commands (`pipe`/`file`/`port` listen + discover) — the subset of
/// [`SharedServerOpts`]'s tuning the directory path reads. Production runs
/// on the `crate::util::consts` defaults; the subprocess suite shortens
/// them and flips `--directory-private` for a hermetic loopback directory.
#[derive(Parser, Debug)]
pub(crate) struct DirectoryTuningArgs {
    /// Peer-eviction silence timeout (seconds) for the directory session.
    #[arg(long, hide = true, default_value_t = consts::ALIVE_TIMEOUT_SECS)]
    pub alive_timeout_secs: u64,

    /// Grace before an unmeshed joiner co-hosts the rendezvous (seconds).
    #[arg(long, hide = true, default_value_t = consts::BEACON_COHOST_GRACE_SECS)]
    pub beacon_cohost_grace_secs: u64,

    /// Directory re-broadcast cadence for an advertiser (seconds).
    #[arg(long, hide = true, default_value_t = consts::ADVERTISE_INTERVAL_SECS)]
    pub advertise_interval_secs: u64,

    /// How long a discoverer keeps showing a ticket after its last ad (seconds).
    #[arg(long, hide = true, default_value_t = consts::DIRECTORY_EXPIRY_SECS)]
    pub directory_expiry_secs: u64,

    /// Use the loopback (private) directory + relax the advertise→reachable guard.
    #[arg(long, hide = true, default_value_t = false)]
    pub directory_private: bool,
}

impl DirectoryTuningArgs {
    /// The process tuning carried by these flags (defaults elsewhere), for
    /// [`crate::util::tuning::init`].
    pub(crate) fn tuning(&self) -> crate::util::tuning::Tuning {
        crate::util::tuning::Tuning {
            alive_timeout_secs: self.alive_timeout_secs,
            cohost_grace_secs: self.beacon_cohost_grace_secs,
            advertise_interval_secs: self.advertise_interval_secs,
            directory_expiry_secs: self.directory_expiry_secs,
            directory_private: self.directory_private,
            ..crate::util::tuning::Tuning::DEFAULTS
        }
    }
}

impl SharedServerOpts {
    /// The process tuning carried by these flags, for [`crate::util::tuning::init`].
    pub(crate) fn tuning(&self) -> crate::util::tuning::Tuning {
        crate::util::tuning::Tuning {
            alive_timeout_secs: self.alive_timeout_secs,
            sweep_interval_secs: self.sweep_interval_secs,
            task_timeout_secs: self.task_timeout_secs,
            task_keepalive_secs: self.task_keepalive_secs,
            cohost_grace_secs: self.beacon_cohost_grace_secs,
            ping_window_secs: self.ping_window_secs,
            ppid_watch_interval_ms: self.ppid_watch_interval_ms,
            heal_stall_threshold_secs: self.heal_stall_threshold_secs,
            starvation_threshold_secs: self.starvation_threshold_secs,
            advertise_interval_secs: self.advertise_interval_secs,
            directory_expiry_secs: self.directory_expiry_secs,
            antientropy_max_resend: self.antientropy_max_resend,
            directory_private: self.directory_private,
            gossip_active_view_capacity: self.active_view_capacity,
            gossip_passive_view_capacity: self.passive_view_capacity,
        }
    }
}
