//! `join` command args: attach to an existing swarm by id/domain/repo.

use clap::Parser;

use crate::protocol::Nickname;
use crate::resolver::JoinTarget;

use super::shared::SharedServerOpts;

#[derive(Parser, Debug)]
pub(crate) struct JoinOpts {
    /// Swarm identifier (ahsw...), a domain (example.com), or a git repo
    /// URL (github.com/user/repo, gitlab.com/user/repo, bitbucket.org/user/repo).
    /// Non-id values are resolved via /.well-known/agent-habilis-swarm.
    /// Classified + syntactically validated at parse (clap `FromStr`).
    pub swarm: JoinTarget,

    /// Optional nickname (random word-word if not provided). A custom
    /// nickname is 1..=32 UTF-8 characters, excluding control chars,
    /// whitespace, and any of / \ < > #.
    #[arg(long)]
    pub nickname: Option<Nickname>,

    /// Accepted only to emit a clear error: the network mode is
    /// encoded in the swarm id, so `join` has no `--public`.
    #[arg(long, hide = true)]
    pub public: bool,

    /// Accepted only to emit a clear error: the swarm name is
    /// encoded in the swarm id, so `join` has no `--name`.
    #[arg(long, hide = true)]
    pub name: Option<String>,

    #[command(flatten)]
    pub shared: SharedServerOpts,
}
