//! `poll` command args: retrieve buffered messages from a running swarm
//! process via IPC.

use clap::Parser;

use crate::protocol::{MessageId, Nickname, SwarmId};

use super::output::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct PollOpts {
    /// Swarm identifier (ahs...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    /// Only return messages after this message ID
    #[arg(long)]
    pub after: Option<MessageId>,

    /// Output format: human (default) or json (structured JSON)
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}
