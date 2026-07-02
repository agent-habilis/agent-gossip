//! `poll` command args: retrieve buffered messages from a running swarm
//! process via IPC.

use clap::Parser;

use crate::protocol::{Nickname, SwarmId};

use super::output::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct PollOpts {
    /// Swarm identifier (🐝...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    /// Only return events surfaced after this sequence number. Omit on the
    /// first poll to get the buffered history; then pass the last returned
    /// event's `seq` to receive only newer events.
    #[arg(long)]
    pub after: Option<u64>,

    /// Block until new events arrive (long-poll). The daemon holds each
    /// request up to ~60s and the CLI transparently re-issues on an empty
    /// window, so this never times out — bound it externally if needed
    /// (e.g. `timeout 15 ahsw poll --long ...`). Omit for an immediate read.
    #[arg(long)]
    pub long: bool,

    /// Output format: human (default) or json (structured JSON)
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}
