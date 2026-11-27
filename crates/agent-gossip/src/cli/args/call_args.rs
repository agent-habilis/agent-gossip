use clap::Args;
use habilis_network::protocol::{MeshId, Nickname};

#[derive(Args, Debug)]
pub(crate) struct CallArgs {
    /// Gossip identifier
    #[arg(long, alias = "room")]
    pub gossip: MeshId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    /// The peer to call.
    #[arg(long)]
    pub to: Nickname,

    /// The A2A JSON-RPC method (e.g. `SendMessage`, `GetTask`,
    /// `ListTasks`, `CancelTask`).
    #[arg(long, default_value = "SendMessage")]
    pub method: String,

    /// Message text — sugar for `SendMessage` (the daemon composes the A2A
    /// Message). Ignored when `--params` is given.
    #[arg(long)]
    pub text: Option<String>,

    /// Task id — the follow-up target for `SendMessage`, or the `id` for
    /// `GetTask` / `CancelTask`. Ignored when `--params` is given.
    #[arg(long)]
    pub task_id: Option<crate::a2a::TaskId>,

    /// One-line name for the task, carried to the peer so both parties
    /// label it identically. Sugar for `SendMessage`; ignored when
    /// `--params` is given.
    #[arg(long)]
    pub label: Option<String>,

    /// The raw JSON-RPC params object. Overrides `--text` / `--task-id` /
    /// `--label`.
    #[arg(long)]
    pub params: Option<String>,

    /// How long to wait for the peer's response, in seconds.
    #[arg(long, default_value_t = crate::a2a::tuning::CALL_TIMEOUT_SECS)]
    pub timeout_secs: u64,

    /// Attach a file to the `SendMessage`, transferred peer-to-peer over
    /// the blob channel and referenced as a Part.url. Works on a new task
    /// and on a `--task-id` follow-up; the peer fetches it with
    /// `agent-gossip a2a fetch <ticket>`.
    #[arg(long)]
    pub file: Option<std::path::PathBuf>,
    /// Filename to advertise for --file (defaults to the file's own name).
    #[arg(long, requires = "file")]
    pub file_name: Option<String>,
    /// MIME type to advertise for --file (e.g. application/x-tar).
    #[arg(long, requires = "file")]
    pub file_mime: Option<String>,
}
