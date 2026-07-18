//! `SharedServerOpts` — the option group flattened into the long-running
//! daemon commands (`create`, `join`, `topic`). Holds only genuinely-local,
//! per-process settings; lookup selection is a gossip-wide property carried in
//! the id (see `create`'s `LookupArgs`). The hidden knobs live in
//! [`TuningOpts`](super::tuning::TuningOpts) so a command that needs the tuning
//! but not the daemon flags (`discover`) can take just those.

use clap::Parser;

use agent_habilis_mesh::util::consts;

use super::legacy::LegacyOutput;
use super::tuning::TuningOpts;

/// Shared options for the daemon commands.
#[derive(Parser, Debug)]
pub(crate) struct SharedServerOpts {
    /// Suppress self-authored messages from stdout (for Monitor use)
    #[arg(long, default_value_t = false)]
    pub filter_self: bool,

    /// Cap on live direct connections (the gossip overlay's active-neighbor
    /// set). The gossip holds at most this many QUIC links and relays to peers
    /// beyond it; gossips up to this size form a full mesh with no membership
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
    /// `{gossip, name, nickname, peer_count, ready, last_updated}` to a
    /// JSON file on every peer-set change and a ~10s heartbeat, and deletes it
    /// on clean shutdown — for external tools (e.g. a shell statusline) to
    /// render live count + liveness without IPC. Defaults to
    /// `<runtime-base>/<mesh-prefix>/<nick>.state.json` (beside the socket +
    /// log); pass this to write elsewhere instead.
    #[arg(long)]
    pub state_file: Option<std::path::PathBuf>,

    /// Deprecated no-op: the daemon is always non-interactive. Accepted (and
    /// hidden) so a stale installed skill degrades to the drift nag rather than
    /// a clap error swallowed by its `> /dev/null 2>&1` launch line — see
    /// [`super::legacy`].
    #[arg(long, hide = true, default_value_t = false)]
    pub no_interactive: bool,

    #[command(flatten)]
    pub legacy_output: LegacyOutput,

    #[command(flatten)]
    pub tuning: TuningOpts,
}
