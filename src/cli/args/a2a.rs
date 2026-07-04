//! `a2a` command args. Two families under one subcommand:
//! - **native A2A messaging** (`a2a call` / `status` / `artifact`) — the
//!   gossip request/response + worker-push surface for swarm tasks; and
//! - **the A2A HTTP tunnel** (`a2a expose` / `connect` / `discover`) — bridge a
//!   local A2A HTTP server to a peer over the swarm (a `📡…` ticket, 1:1).

use clap::{Parser, Subcommand};

use super::lookup::PublicLookupArgs;
use super::output::OutputFormat;
use crate::protocol::swarm::SwarmName;
use crate::protocol::{Nickname, SwarmId};

#[derive(Parser, Debug)]
pub(crate) struct A2aOpts {
    #[command(subcommand)]
    pub action: A2aAction,
}

#[derive(Subcommand, Debug)]
pub(crate) enum A2aAction {
    /// Bridge a local A2A server to a peer; prints a `📡…` ticket on stdout.
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
        /// with `ahsw a2a discover` — no `📡…` to copy. Bare `--advertise` ⇒ the
        /// default `global` directory; `--advertise <name>` ⇒ that named
        /// directory (share the name with the peer). The ad carries the full
        /// bearer ticket, so pair it with `--password`.
        #[arg(long, num_args(0..=1))]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct directory states (see DirectorySelection)"
        )]
        advertise: Option<Option<SwarmName>>,

        /// Protect the bridge with a password: the ticket alone no longer
        /// redeems — consumers must present the password (so a passworded ticket
        /// is safe to share). Bare `--password` prompts hidden on the terminal;
        /// `--password=<pw>` passes it inline (visible in `ps` — prefer the
        /// prompt when a human types it).
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,

        /// Force loopback-only lookups so two agents on one machine bridge
        /// hermetically off the ticket's direct address (no mDNS/DHT/relay).
        /// Hidden — a testing knob.
        #[arg(long, hide = true, default_value_t = false)]
        loopback: bool,

        /// Output format: human (default) or json (a bare connect line).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },

    /// Redeem a `📡…` ticket and bind a local A2A endpoint for a client.
    ///
    /// Binds `127.0.0.1:PORT` (an ephemeral port unless `--port` is given) that
    /// an unmodified A2A client/SDK points at; forwards every request to the
    /// exposer over the swarm and rewrites the Agent Card so the client
    /// discovers the local bridge, not the unreachable origin.
    Connect {
        /// The `📡…` ticket printed by `ahsw a2a expose`.
        ticket: String,

        /// Local port to bind the bridge on (default: an ephemeral port — the
        /// bound URL is printed).
        #[arg(long)]
        port: Option<u16>,

        /// Password for a password-protected ticket — required exactly when the
        /// ticket carries the password flag (prompts on a terminal, or inline
        /// via `--password=<pw>`).
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,

        /// Output format: human (default) or json (prints the bound URL only).
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },

    /// Browse a directory for advertised a2a bridges and connect to one — the
    /// receiver side of `a2a expose --advertise`, no `📡…` to copy.
    ///
    /// Human mode runs a live picker and binds a local endpoint on selection;
    /// `--output json` streams `ticket_found`/`ticket_lost` lines instead (the
    /// agent captures a ticket and runs `a2a connect` itself).
    Discover {
        /// The directory to browse — the name the exposer passed to
        /// `--advertise` (omit for the default `global` directory).
        #[arg(long)]
        directory: Option<SwarmName>,

        /// Local port to bind the bridge on when a bridge is picked (default: an
        /// ephemeral port).
        #[arg(long)]
        port: Option<u16>,

        /// Which lookup mechanisms reach the directory (same flags as
        /// `discover`): must match the advertiser's. Naming none (or `--public`)
        /// is the all-on public preset.
        #[command(flatten)]
        lookups: PublicLookupArgs,

        /// Password for a password-protected pick (🔒 in the picker) — prompts
        /// on pick when omitted.
        #[arg(long, num_args(0..=1), require_equals = true)]
        #[expect(
            clippy::option_option,
            reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
        )]
        password: Option<Option<String>>,

        /// Output format: human (default) — the live picker — or json, one
        /// `ticket_found`/`ticket_lost` line per directory change.
        #[arg(long, default_value = "human")]
        output: OutputFormat,
    },

    /// Make an A2A `SendMessage` (or `tasks/*`) call.
    ///
    /// With `--to <peer>` this is a directed request/response over gossip: the
    /// peer serves a **safe method subset** — `GetTask`, `ListTasks`,
    /// `CancelTask` (a task you're a party to), `swarm/state.get`,
    /// `swarm/meta.get`, and `SendMessage` (task creation: no `--task-id`
    /// opens a task the peer mints and returns; `--task-id` is a follow-up).
    /// Mutating global ops are refused.
    ///
    /// **Without `--to`** and `--method SendMessage`, `--text` is broadcast to
    /// the whole swarm (A2A is point-to-point, so a swarm-wide message declares
    /// itself). Exits non-zero when the response is an error or times out.
    Call {
        /// Swarm identifier (🐝...)
        #[arg(long)]
        swarm: SwarmId,

        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,

        /// The peer to call. Omit for a swarm broadcast `SendMessage`.
        #[arg(long)]
        to: Option<Nickname>,

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

        /// The raw JSON-RPC params object. Overrides `--text` / `--task-id`.
        #[arg(long)]
        params: Option<String>,

        /// How long to wait for the peer's response, in seconds.
        #[arg(long, default_value_t = 15)]
        timeout_secs: u64,
    },

    /// Worker-emit a task `TaskStatusUpdate` (the A2A streaming plane): move a
    /// task you're serving to `working` / `input-required` / `completed` /
    /// `failed`. Pushed fire-and-forget to the other party.
    Status {
        /// Swarm identifier (🐝...)
        #[arg(long)]
        swarm: SwarmId,
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
        /// Swarm identifier (🐝...)
        #[arg(long)]
        swarm: SwarmId,
        /// Nickname of the local agent (must have a running join/create session)
        #[arg(long)]
        nickname: Nickname,
        /// The task id (uuid).
        #[arg(long)]
        task_id: crate::a2a::TaskId,
        /// The result text.
        #[arg(long)]
        text: String,
    },
}

/// Parse an A2A task state from its friendly (kebab-case) name — the agent
/// surface, not the A2A wire's `ProtoJSON` `TASK_STATE_*`.
fn parse_state(raw: &str) -> Result<crate::a2a::TaskState, String> {
    crate::a2a::TaskState::from_friendly(raw).ok_or_else(|| {
        format!("invalid task state '{raw}' (working|input-required|completed|failed|canceled)")
    })
}
