use clap::Subcommand;
use habilis_network::protocol::{MeshName, Nickname};

use super::artifact_args::ArtifactArgs;
use super::call_args::CallArgs;
use super::expose_args::ExposeArgs;
use super::legacy_output::LegacyOutput;
use super::lookup::LookupArgs;
use super::session_address::SessionAddress;
use super::status_args::StatusArgs;
use crate::cli::password_flag::{PasswordFlag, password_flag_parser};

#[derive(Subcommand, Debug)]
pub(crate) enum A2aAction {
    /// Bridge a local A2A server to a peer; prints a ticket on stdout.
    ///
    /// Binds an endpoint next to the A2A server named by `--to` and forwards
    /// each connecting peer's requests to it, rewriting the Agent Card's URLs so
    /// discovery resolves through the tunnel. Strictly 1:1 — one consumer is
    /// served at a time; a second is refused until the first disconnects.
    Expose(ExposeArgs),

    /// Redeem a ticket and bind a local A2A endpoint for a client.
    ///
    /// Binds `127.0.0.1:PORT` (an ephemeral port unless `--port` is given) that
    /// an unmodified A2A client/SDK points at; forwards every request to the
    /// exposer over the gossip and rewrites the Agent Card so the client
    /// discovers the local bridge, not the unreachable origin.
    Connect {
        /// The ticket printed by `agent-gossip a2a expose`.
        ticket: String,

        /// Local port to bind the bridge on (default: an ephemeral port — the
        /// bound URL is printed).
        #[arg(long)]
        port: Option<u16>,

        /// Password for a password-protected ticket — required exactly when the
        /// ticket carries the password flag. Pass it inline via `--password=<pw>`.
        #[arg(long, num_args(0..=1), require_equals = true, default_missing_value = "\0", value_parser = password_flag_parser())]
        password: Option<PasswordFlag>,

        /// Serve any local client, without the per-bridge token printed on
        /// startup. The bridge presents the ticket credential itself, so this
        /// hands its access to every process on the machine — use it only when
        /// the client cannot send a header. Cross-origin requests stay refused
        /// either way.
        #[arg(long)]
        allow_anonymous: bool,

        #[command(flatten)]
        legacy_output: LegacyOutput,
    },

    /// Browse a directory for advertised a2a bridges — the receiver side of
    /// `a2a expose --advertise`, no ticket to copy.
    ///
    /// Streams one `ticket_found`/`ticket_lost` JSON line per directory change;
    /// the agent captures a ticket and runs `a2a connect` itself.
    Discover {
        /// The directory to browse — the name the exposer passed to
        /// `--advertise` (omit for the default `global` directory).
        #[arg(long)]
        directory: Option<MeshName>,

        /// Which lookup mechanisms reach the directory (same flags as
        /// `discover`): must match the advertiser's. Naming none is the
        /// all-on public preset.
        #[command(flatten)]
        lookups: LookupArgs,

        #[command(flatten)]
        legacy_output: LegacyOutput,
    },

    /// Send a chat message to the whole gossip.
    ///
    /// Every member sees it. Use `a2a msg` to reach one peer privately.
    Broadcast {
        #[command(flatten)]
        session: SessionAddress,

        /// Message text.
        #[arg(long)]
        text: String,
    },

    /// Send a msg: a chat message to one peer.
    ///
    /// Only you and `--to` see it: the frame is delivered point-to-point and
    /// sealed to the recipient, so the peers relaying it cannot read the body.
    /// This is chat, not a task — use `a2a call --to <peer>` to delegate work.
    Msg {
        #[command(flatten)]
        session: SessionAddress,

        /// The peer to message.
        #[arg(long)]
        to: Nickname,

        /// Message text.
        #[arg(long)]
        text: String,
    },

    /// Make an A2A `SendMessage` (or `tasks/*`) call to a peer.
    ///
    /// A directed request/response over gossip: the peer serves a **safe method
    /// subset** — `GetTask`, `ListTasks`, `CancelTask` (a task you're a party
    /// to), `mesh/state.get`, `mesh/meta.get`, and `SendMessage` (task
    /// creation: no `--task-id` opens a task the peer mints and returns;
    /// `--task-id` is a follow-up). Mutating global ops are refused.
    /// `--file` attaches a file to a `SendMessage`, as `a2a artifact --file`
    /// does.
    ///
    /// This always opens or advances a **task**. For chat, use `a2a broadcast`
    /// or `a2a msg`. Exits non-zero when the response is an error or times out.
    Call(CallArgs),

    /// Worker-emit a task `TaskStatusUpdate` (the A2A streaming plane): move a
    /// task you're serving to `working` / `input-required` / `completed` /
    /// `failed`. Pushed fire-and-forget to the other party.
    Status(StatusArgs),

    /// Worker-emit a task `TaskArtifactUpdate` (the result) for a task you're
    /// serving. Parks the task in `input-required` for the initiator's approval.
    Artifact(ArtifactArgs),

    /// Fetch a blob artifact by its ticket reference (the `url` of a received
    /// file part). A direct peer-to-peer transfer, streamed to disk. With
    /// `--nickname` it lands in that session's `<nick>.recv/` folder (named by
    /// the content hash) and prints the path; otherwise it streams to stdout —
    /// redirect or pipe, e.g. `agent-gossip a2a fetch <ticket> > report.pdf`.
    Fetch {
        /// The blob ticket copied from a received file part's `url`.
        ticket: String,
        /// Land the file under this session's `<nick>.recv/` folder instead of
        /// streaming to stdout. Resolves the session's temp dir by nickname.
        #[arg(long)]
        nickname: Option<Nickname>,
        /// Write the bytes to this path instead of the default. `-` forces
        /// stdout (useful for pipelines even with `--nickname`).
        #[arg(long, short)]
        output: Option<std::path::PathBuf>,
        /// Password, if the ticket is password-protected.
        #[arg(long)]
        password: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::A2aAction;
    use crate::a2a::tuning;
    use crate::cli::args::{Cli, Commands};

    /// `--timeout-secs` must default to the shared constant, not a literal.
    /// `a2a::tuning`'s own test pins the other two surfaces.
    #[test]
    fn the_call_timeout_default_is_the_shared_constant() {
        let cli = Cli::parse_from([
            "agent-gossip",
            "a2a",
            "call",
            "--gossip",
            "3CyrP8nj82n8e1Cnr33xKEkssHsdFfKDDVNfqyFzZkS2ENuUjtywLgW6hcdRM",
            "--nickname",
            "calm-otter",
            "--to",
            "wire-thistle",
            "--method",
            "SendMessage",
        ]);
        let Commands::A2a { action } = cli.command else {
            panic!("expected the a2a command");
        };
        let A2aAction::Call(call) = action else {
            panic!("expected the call action");
        };
        assert_eq!(
            call.timeout_secs,
            tuning::CALL_TIMEOUT_SECS,
            "a2a call defaults its timeout to the shared constant"
        );
    }
}
