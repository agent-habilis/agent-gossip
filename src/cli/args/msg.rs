//! `msg` command args: post one message to a swarm via the running
//! daemon's IPC socket.

use clap::Parser;

use crate::protocol::{MessageBody, Nickname, SwarmId};

#[derive(Parser, Debug)]
pub(crate) struct MsgOpts {
    /// Swarm identifier (ahs...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent to post as (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    /// The message text (UTF-8; newlines/tabs allowed, other control
    /// characters rejected)
    #[arg(long)]
    pub text: MessageBody,

    /// Address this message to a specific peer's nickname
    #[arg(long)]
    pub reply: Option<Nickname>,
}
