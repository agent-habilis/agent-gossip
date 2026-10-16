//! `TuningOpts` — the hidden per-process knobs, split out of
//! [`SharedServerOpts`](super::shared::SharedServerOpts) so a command can take
//! the tuning without also advertising the daemon flags it does not honor.
//! `discover` is the case in point: it needs the knobs (the suite varies its
//! directory cadence) but writes no state file and runs no gossip session, so
//! flattening the full server group would list `--state-file` / `--max-peers` /
//! `--filter-self` / `--a2a-serve` in its `--help` as silent no-ops.
//!
//! Not in `--help`. Production runs on the `habilis_network::util::tuning`
//! defaults; the subprocess test suite passes these to run with short timings.
//! These replace the former env-var overrides — see
//! `habilis_network::util::tuning`.

use std::time::Duration;

use clap::Parser;
use habilis_network::util::clock::millis_saturating;
use habilis_network::util::tuning as defaults;

#[derive(Parser, Debug)]
pub(crate) struct TuningOpts {
    /// Peer-eviction silence timeout (seconds).
    #[arg(long, hide = true, default_value_t = defaults::ALIVE_TIMEOUT.as_secs())]
    pub alive_timeout_secs: u64,

    /// How often the sweeper scans for expired peers (seconds).
    #[arg(long, hide = true, default_value_t = defaults::SWEEP_INTERVAL.as_secs())]
    pub sweep_interval_secs: u64,

    /// Cadence of the unconditional gossip healer (seconds).
    #[arg(long, hide = true, default_value_t = defaults::HEAL_INTERVAL.as_secs())]
    pub heal_interval_secs: u64,

    /// Task idle-debounce timeout (seconds).
    #[arg(long, hide = true, default_value_t = crate::a2a::tuning::TASK_TIMEOUT_SECS)]
    pub task_timeout_secs: u64,

    /// Task keepalive cadence, for each party of a live task (seconds).
    #[arg(long, hide = true, default_value_t = crate::a2a::tuning::TASK_KEEPALIVE_SECS)]
    pub task_keepalive_secs: u64,

    /// Task silence from both agents after which the daemons stop beating it (seconds).
    #[arg(long, hide = true, default_value_t = crate::a2a::tuning::TASK_SKILL_SILENCE_MAX_SECS)]
    pub task_skill_silence_max_secs: u64,

    /// Worker-agent silence after which the initiator's daemon raises `task_stalled` (seconds).
    #[arg(long, hide = true, default_value_t = crate::a2a::tuning::TASK_STALL_CHECK_SECS)]
    pub task_stall_check_secs: u64,

    /// Grace before an unmeshed joiner co-hosts the rendezvous (seconds).
    #[arg(long, hide = true, default_value_t = defaults::BEACON_COHOST_GRACE.as_secs())]
    pub beacon_cohost_grace_secs: u64,

    /// How long an `agent-gossip ping` round collects pongs (seconds).
    #[arg(long, hide = true, default_value_t = defaults::PING_WINDOW.as_secs())]
    pub ping_window_secs: u64,

    /// How often the daemon checks for orphaning by its spawning agent (millis).
    #[arg(long, hide = true, default_value_t = millis_saturating(defaults::PPID_WATCH_INTERVAL))]
    pub ppid_watch_interval_ms: u64,

    /// How long a `long: true` poll read parks before returning empty (millis).
    #[arg(long, hide = true, default_value_t = crate::a2a::tuning::LONGPOLL_MAX_MS)]
    pub longpoll_max_ms: u64,

    /// Heal inter-tick gap above which the process hard re-bootstraps (seconds).
    #[arg(long, hide = true, default_value_t = defaults::HEAL_STALL_THRESHOLD.as_secs())]
    pub heal_stall_threshold_secs: u64,

    /// No inbound gossip for this long, with peers known, trips the starvation watchdog (seconds).
    #[arg(long, hide = true, default_value_t = defaults::STARVATION_THRESHOLD.as_secs())]
    pub starvation_threshold_secs: u64,

    /// Directory re-broadcast cadence for an advertiser (seconds).
    #[arg(long, hide = true, default_value_t = defaults::ADVERTISE_INTERVAL.as_secs())]
    pub advertise_interval_secs: u64,

    /// How long a discoverer keeps showing a gossip after its last ad (seconds).
    #[arg(long, hide = true, default_value_t = defaults::DIRECTORY_EXPIRY.as_secs())]
    pub directory_expiry_secs: u64,

    /// How often a member broadcasts its anti-entropy digest (seconds).
    #[arg(long, hide = true, default_value_t = defaults::ANTIENTROPY_INTERVAL.as_secs())]
    pub antientropy_interval_secs: u64,

    /// Max messages re-sent in response to one anti-entropy digest.
    #[arg(long, hide = true, default_value_t = defaults::ANTIENTROPY_MAX_RESEND)]
    pub antientropy_max_resend: usize,

    /// Use the loopback (private) directory + relax the advertise→public guard.
    #[arg(long, hide = true, default_value_t = false)]
    pub directory_private: bool,

    /// First rival re-check shed of an `EagerProbed` public beacon (seconds).
    #[arg(long, hide = true, default_value_t = defaults::RIVAL_RECHECK_FIRST.as_secs())]
    pub rival_recheck_first_secs: u64,

    /// Steady rival re-check cadence for a lone beacon holder (seconds).
    #[arg(long, hide = true, default_value_t = defaults::RIVAL_RECHECK.as_secs())]
    pub rival_recheck_secs: u64,

    /// Steady rival re-check cadence while meshed (seconds).
    #[arg(long, hide = true, default_value_t = defaults::RIVAL_RECHECK_MESHED.as_secs())]
    pub rival_recheck_meshed_secs: u64,

    /// Rival re-check cadence ceiling for a beacon holder with no peers (seconds).
    #[arg(long, hide = true, default_value_t = defaults::RIVAL_RECHECK_ALONE.as_secs())]
    pub rival_recheck_alone_secs: u64,

    /// How long a member that let go of the rendezvous waits before it comes back to it (seconds).
    #[arg(long, hide = true, default_value_t = defaults::RENDEZVOUS_PERIODIC_RETURN.as_secs())]
    pub rendezvous_periodic_return_secs: u64,

    /// Narrow topic-mesh lookups to mDNS only (no DHT, no relay).
    #[arg(long, hide = true, default_value_t = false)]
    pub topic_mdns_only: bool,
}

impl TuningOpts {
    /// The process tuning carried by these flags, for [`habilis_network::runtime::tuning::init`].
    pub(crate) fn tuning(&self) -> habilis_network::runtime::tuning::Tuning {
        habilis_network::runtime::tuning::Tuning {
            alive_timeout: Duration::from_secs(self.alive_timeout_secs),
            sweep_interval: Duration::from_secs(self.sweep_interval_secs),
            heal_interval: Duration::from_secs(self.heal_interval_secs),
            cohost_grace: Duration::from_secs(self.beacon_cohost_grace_secs),
            ping_window: Duration::from_secs(self.ping_window_secs),
            ppid_watch_interval: Duration::from_millis(self.ppid_watch_interval_ms),
            heal_stall_threshold: Duration::from_secs(self.heal_stall_threshold_secs),
            starvation_threshold: Duration::from_secs(self.starvation_threshold_secs),
            advertise_interval: Duration::from_secs(self.advertise_interval_secs),
            directory_expiry: Duration::from_secs(self.directory_expiry_secs),
            antientropy_interval: Duration::from_secs(self.antientropy_interval_secs),
            antientropy_max_resend: self.antientropy_max_resend,
            directory_private: self.directory_private,
            rival_recheck_first: Duration::from_secs(self.rival_recheck_first_secs),
            rival_recheck: Duration::from_secs(self.rival_recheck_secs),
            rival_recheck_meshed: Duration::from_secs(self.rival_recheck_meshed_secs),
            rival_recheck_alone: Duration::from_secs(self.rival_recheck_alone_secs),
            rendezvous_periodic_return: Duration::from_secs(self.rendezvous_periodic_return_secs),
            topic_mdns_only: self.topic_mdns_only,
        }
    }

    /// The A2A-layer tuning carried by these flags, for
    /// [`crate::a2a::tuning::init`]. Separate from [`Self::tuning`] because the
    /// task state machine and the long-poll park are implemented here, not in
    /// the engine — one flag set, two homes.
    pub(crate) fn a2a_tuning(&self) -> crate::a2a::tuning::Tuning {
        crate::a2a::tuning::Tuning {
            task_timeout_secs: self.task_timeout_secs,
            task_keepalive_secs: self.task_keepalive_secs,
            task_skill_silence_max_secs: self.task_skill_silence_max_secs,
            task_stall_check_secs: self.task_stall_check_secs,
            longpoll_max_ms: self.longpoll_max_ms,
        }
    }
}
