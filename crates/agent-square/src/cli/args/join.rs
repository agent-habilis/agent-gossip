//! `join` command args: attach to an existing square by id/domain/repo.

use clap::Parser;

use crate::cli::password::PasswordFlag;
use agent_habilis_mesh::protocol::Nickname;
use agent_habilis_mesh::resolver::JoinTarget;

use super::shared::SharedServerOpts;

#[derive(Parser, Debug)]
pub(crate) struct JoinOpts {
    /// Square identifier (💬...). Validated at parse (clap `FromStr`). For a
    /// public square derived from a shared string, use `agent-square topic <string>`.
    pub square: JoinTarget,

    /// Optional nickname (random word-word if not provided). A custom
    /// nickname is 1..=32 UTF-8 characters, excluding control chars,
    /// whitespace, and any of / \ < > #.
    #[arg(long)]
    pub nickname: Option<Nickname>,

    /// Accepted only to emit a clear error: the network mode is
    /// encoded in the square id, so `join` has no `--public`.
    #[arg(long, hide = true)]
    pub public: bool,

    /// Accepted only to emit a clear error: the square name is
    /// encoded in the square id, so `join` has no `--name`.
    #[arg(long, hide = true)]
    pub name: Option<String>,

    /// Password for a password-protected square id — required exactly when
    /// the id carries a password verifier (checked locally before any
    /// network; a wrong password fails immediately). Pass it inline as
    /// `--password=<pw>`; a bare `--password`, or omitting the flag on a
    /// protected id, is an error (there is no terminal prompt).
    #[arg(long, num_args(0..=1), require_equals = true, default_missing_value = "\0")]
    pub password: Option<PasswordFlag>,

    #[command(flatten)]
    pub shared: SharedServerOpts,
}
