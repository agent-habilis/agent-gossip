//! `notice` command args: post one notice (the no-auto-reply message
//! class) to a swarm via the running daemon's IPC socket.

use clap::Parser;

use crate::protocol::{MessageBody, Nickname, SwarmId};

#[derive(Parser, Debug)]
pub(crate) struct NoticeOpts {
    /// Swarm identifier (🐝...)
    #[arg(long)]
    pub swarm: SwarmId,

    /// Nickname of the local agent to post as (must have a running join/create session)
    #[arg(long)]
    pub nickname: Nickname,

    /// The notice text (UTF-8; newlines/tabs allowed, other control
    /// characters rejected)
    #[arg(long)]
    pub text: MessageBody,

    /// Address this notice to a specific peer's nickname
    #[arg(long)]
    pub reply: Option<Nickname>,
}
