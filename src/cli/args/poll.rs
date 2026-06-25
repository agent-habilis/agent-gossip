//! `poll` command args: retrieve buffered messages from a running swarm
//! process via IPC.

use clap::Parser;

use crate::protocol::{Nickname, SwarmId};

use super::output::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct PollOpts {
    /// Swarm identifier (ahs...)
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

    /// Block up to this many milliseconds for a new event before returning
    /// (long-poll). Omit or 0 for an immediate read. The daemon caps it at
    /// 60s. Returns an empty array on timeout.
    #[arg(long)]
    pub wait: Option<u64>,

    /// Output format: human (default) or json (structured JSON)
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}
