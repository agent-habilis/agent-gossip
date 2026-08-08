//! `a2a` command args. Two families under one subcommand:
//! - **native A2A messaging** (`a2a call` / `status` / `artifact`) — the
//!   gossip request/response + worker-push surface for gossip tasks; and
//! - **the A2A HTTP tunnel** (`a2a expose` / `connect` / `discover`) — bridge a
//!   local A2A HTTP server to a peer over the gossip (a ticket, 1:1).

use clap::{Parser, Subcommand};

use super::legacy::LegacyOutput;
use super::lookup::PublicLookupArgs;
use crate::cli::password::PasswordFlag;
use fofoca::protocol::MeshName;
use fofoca::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct A2aOpts {
    #[command(subcommand)]
    pub action: A2aAction,
}

#[derive(Subcommand, Debug)]
pub(crate) enum A2aAction {
    /// Bridge a local A2A server to a peer; prints a ticket on stdout.
    ///
    /// Binds an endpoint next to the A2A server named by `--to` and forwards
    /// each connecting peer's requests to it, rewriting the Agent Card's URLs so
    /// discovery resolves through the tunnel. Strictly 1:1 — one consumer is
    /// served at a time; a second is refused until the first disconnects.
    Expose {
        /// The local A2A origin to bridge, as a plain http URL (e.g.
        /// `http://127.0.0.1:9999`). https and path-prefixed origins are out of
        /// scope — the bridge forwards raw TCP to a plain-http localhost origin.
        #[arg(long)]
        to: String,

        /// Which lookup mechanisms the bridge uses (same flags as `create`):
        /// naming any uses only those; naming none (or `--public`) is the all-on
        /// public preset.
        #[command(flatten)]
        lookups: PublicLookupArgs,

        /// Advertise this bridge's ticket in a directory so a peer can find it
        /// with `agent-gossip a2a discover` — no ticket to copy. Bare `--advertise` ⇒ the
        /// default `global` directory; `--advertise <name>` ⇒ that named
        /// directory (share the name with the peer). The ad carries the full
        /// bearer ticket, so pair it with `--password`.
        #[arg(long, num_args(0..=1), default_missing_value = "global")]
        advertise: Option<MeshName>,

        /// Protect the bridge with a password: the ticket alone no longer
        /// redeems — consumers must present the password (so a passworded ticket
        /// is safe to share). Pass it inline as `--password=<pw>`.
        #[arg(long, num_args(0..=1), require_equals = true, default_missing_value = "\0")]
        password: Option<PasswordFlag>,

        /// Force loopback-only lookups so two agents on one machine bridge
        /// hermetically off the ticket's direct address (no mDNS/DHT/relay).
        /// Hidden — a testing knob.
        #[arg(long, hide = true, default_value_t = false)]
        loopback: bool,

        #[command(flatten)]
        legacy_output: LegacyOutput,
    },

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
        #[arg(long, num_args(0..=1), require_equals = true, default_missing_value = "\0")]
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
        /// `discover`): must match the advertiser's. Naming none (or `--public`)
        /// is the all-on public preset.
        #[command(flatten)]
        lookups: PublicLookupArgs,

        #[command(flatten)]
        legacy_output: LegacyOutput,
    },

    /// Send a chat message to the whole gossip.
    ///
    /// Every member sees it. Use `a2a msg` to reach one peer privately.
    Broadcast {
        /// Gossip identifier
        #[arg(long, alias = "room")]
        gossip: MeshId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,

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
        /// Gossip identifier
        #[arg(long, alias = "room")]
        gossip: MeshId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,

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
    ///
    /// This always opens or advances a **task**. For chat, use `a2a broadcast`
    /// or `a2a msg`. Exits non-zero when the response is an error or times out.
    Call {
        /// Gossip identifier
        #[arg(long, alias = "room")]
        gossip: MeshId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,

        /// The peer to call.
        #[arg(long)]
        to: Nickname,

        /// The A2A JSON-RPC method (e.g. `SendMessage`, `GetTask`,
        /// `ListTasks`, `CancelTask`).
        #[arg(long, default_value = "SendMessage")]
        method: String,

        /// Message text — sugar for `SendMessage` (the daemon composes the A2A
        /// Message). Ignored when `--params` is given.
        #[arg(long)]
        text: Option<String>,

        /// Task id — the follow-up target for `SendMessage`, or the `id` for
        /// `GetTask` / `CancelTask`. Ignored when `--params` is given.
        #[arg(long)]
        task_id: Option<crate::a2a::TaskId>,

        /// One-line name for the task, carried to the peer so both parties
        /// label it identically. Sugar for `SendMessage`; ignored when
        /// `--params` is given.
        #[arg(long)]
        label: Option<String>,

        /// The raw JSON-RPC params object. Overrides `--text` / `--task-id` /
        /// `--label`.
        #[arg(long)]
        params: Option<String>,

        /// How long to wait for the peer's response, in seconds.
        #[arg(long, default_value_t = crate::a2a::tuning::CALL_TIMEOUT_SECS)]
        timeout_secs: u64,
    },

    /// Worker-emit a task `TaskStatusUpdate` (the A2A streaming plane): move a
    /// task you're serving to `working` / `input-required` / `completed` /
    /// `failed`. Pushed fire-and-forget to the other party.
    Status {
        /// Gossip identifier
        #[arg(long, alias = "room")]
        gossip: MeshId,
        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,
        /// The task id (uuid).
        #[arg(long)]
        task_id: crate::a2a::TaskId,
        /// The new A2A state.
        #[arg(long, value_parser = parse_state)]
        state: crate::a2a::TaskState,
        /// Optional message text (becomes the status message — a question, a
        /// completion summary, a failure reason).
        #[arg(long = "text", alias = "note")]
        note: Option<String>,
    },

    /// Worker-emit a task `TaskArtifactUpdate` (the result) for a task you're
    /// serving. Parks the task in `input-required` for the initiator's approval.
    Artifact {
        /// Gossip identifier
        #[arg(long, alias = "room")]
        gossip: MeshId,
        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,
        /// The task id (uuid).
        #[arg(long)]
        task_id: crate::a2a::TaskId,
        /// The result text (optional when --file is given).
        #[arg(long)]
        text: Option<String>,
        /// Attach a file as the result, transferred peer-to-peer over the blob
        /// channel and referenced as a Part.url. For binaries too large to
        /// inline; the receiver fetches it with `agent-gossip a2a fetch <ticket>`.
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// Filename to advertise for --file (defaults to the file's own name).
        #[arg(long)]
        file_name: Option<String>,
        /// MIME type to advertise for --file (e.g. application/pdf).
        #[arg(long)]
        file_mime: Option<String>,
    },

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

/// Parse an A2A task state from its friendly (kebab-case) name — the agent
/// surface, not the A2A wire's `ProtoJSON` `TASK_STATE_*`.
fn parse_state(raw: &str) -> Result<crate::a2a::TaskState, String> {
    crate::a2a::TaskState::from_friendly(raw).ok_or_else(|| {
        format!("invalid task state '{raw}' (working|input-required|completed|failed|canceled)")
    })
}
