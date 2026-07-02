//! `join` command args: attach to an existing swarm by id/domain/repo.

use clap::Parser;

use crate::protocol::Nickname;
use crate::resolver::JoinTarget;

use super::shared::SharedServerOpts;

#[derive(Parser, Debug)]
pub(crate) struct JoinOpts {
    /// Swarm identifier (🐝...). Validated at parse (clap `FromStr`). For a
    /// public swarm derived from a shared string, use `ahsw forum <string>`.
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

    /// Password for a password-protected swarm id — required exactly when
    /// the id carries a password verifier (checked locally before any
    /// network; a wrong password fails immediately). Bare `--password`
    /// prompts hidden on the terminal (as does omitting the flag entirely
    /// for a protected id); `--password=<pw>` passes it inline (visible in
    /// `ps` and shell history — prefer the prompt when a human types it).
    #[arg(long, num_args(0..=1), require_equals = true)]
    #[expect(
        clippy::option_option,
        reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
    )]
    pub password: Option<Option<String>>,

    #[command(flatten)]
    pub shared: SharedServerOpts,
}
