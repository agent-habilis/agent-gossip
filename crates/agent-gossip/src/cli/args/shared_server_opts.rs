//! `SharedServerOpts` — the option group flattened into the long-running
//! daemon commands (`create`, `join`, `topic`). Holds only genuinely-local,
//! per-process settings; lookup selection is a gossip-wide property carried in
//! the id (see `create`'s `LookupArgs`). The hidden knobs live in
//! [`TuningOpts`] so a command that needs the tuning
//! but not the daemon flags (`discover`) can take just those.

use clap::Parser;
use habilis_network::util::tuning as defaults;

use super::legacy_output::LegacyOutput;
use super::tuning_opts::TuningOpts;

/// Shared options for the daemon commands.
#[derive(Parser, Debug)]
pub(crate) struct SharedServerOpts {
    /// Suppress self-authored messages from stdout (for Monitor use)
    #[arg(long, default_value_t = false)]
    pub filter_self: bool,

    /// Cap on direct connections, gossip neighbors included. If the transport
    /// policy has multihop and a route is known, a peer past the cap is reached
    /// through a connected peer. Pinned and extra connections fit up to twice
    /// this value.
    #[arg(long, default_value_t = defaults::MAX_CONNECTIONS)]
    pub max_connections: usize,

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
    /// log); pass this to write elsewhere instead. `leave` and `session` find
    /// a file outside the runtime base through the daemon's socket. If the
    /// daemon dies without cleaning up (SIGKILL, power loss), nothing names
    /// that file any more, so it stays.
    #[arg(long)]
    pub state_file: Option<std::path::PathBuf>,

    /// Run the daemon detached from the launching process, owned by this pid.
    /// The launcher exits at once; the daemon stops within seconds after the
    /// owner process exits. Use the pid of the agent that owns the session.
    #[arg(long, value_name = "PID")]
    pub owner_pid: Option<u32>,

    /// Internal marker: this process is the detached child that
    /// `--owner-pid` re-spawned.
    #[arg(long, hide = true, default_value_t = false)]
    pub detached_child: bool,

    /// Deprecated no-op: the daemon is always non-interactive. Accepted (and
    /// hidden) so a stale installed skill degrades to the drift nag rather than
    /// a clap error swallowed by its `> /dev/null 2>&1` launch line — see
    /// `super::legacy`.
    #[arg(long, hide = true, default_value_t = false)]
    pub no_interactive: bool,

    #[command(flatten)]
    pub legacy_output: LegacyOutput,

    #[command(flatten)]
    pub tuning: TuningOpts,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn the_daemon_commands_default_to_16_direct_connections() {
        let cli = Cli::try_parse_from(["agent-gossip", "create"]).expect("a bare create parses");
        let Commands::Create { options } = cli.command else {
            panic!("expected Create command");
        };
        assert_eq!(
            options.shared.max_connections, 16,
            "the flag defaults to 16 direct connections"
        );
    }
}
