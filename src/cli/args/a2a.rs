//! `a2a call` command args: send a directed A2A JSON-RPC request to a peer
//! over gossip and print its response — the gossip request/response binding.

use clap::{Parser, Subcommand};

use crate::protocol::{Nickname, SwarmId};

#[derive(Parser, Debug)]
pub(crate) struct A2aOpts {
    #[command(subcommand)]
    pub action: A2aAction,
}

#[derive(Subcommand, Debug)]
pub(crate) enum A2aAction {
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
