//! `invite` command args: mint a `🎟️` invite to an invite-only square. Only the
//! creating session's daemon holds the in-memory issuer key, so only it can
//! sign one — after its restart, no new invites can be minted.

use clap::Parser;

use super::legacy::LegacyOutput;

use agent_habilis_mesh::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct InviteOpts {
    /// The invite-only square to mint for (its 💬… id).
    #[arg(long)]
    pub square: MeshId,

    /// Nickname of the local **creating** session (only its daemon holds the
    /// issuer key that can sign an invite).
    #[arg(long)]
    pub nickname: Nickname,

    /// Invite lifetime before it stops admitting: a duration like `1h`, `30m`,
    /// `7d`, or a bare number of seconds. `none` (or `0`) mints a no-expiry
    /// invite. Defaults to 24h.
    #[arg(long)]
    pub ttl: Option<String>,

    #[command(flatten)]
    pub legacy_output: LegacyOutput,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    #[test]
    fn invite_parses_mesh_nickname_and_ttl() {
        let cli = Cli::parse_from([
            "agent-square",
            "invite",
            "--square",
            "💬AbCdEf1234",
            "--nickname",
            "my-nick",
            "--ttl",
            "1h",
        ]);
        let Commands::Invite { opts } = cli.command else {
            panic!("expected Invite command");
        };
        assert_eq!(opts.ttl.as_deref(), Some("1h"));
    }

    #[test]
    fn invite_ttl_is_optional() {
        let cli = Cli::parse_from([
            "agent-square",
            "invite",
            "--square",
            "💬AbCdEf1234",
            "--nickname",
            "my-nick",
        ]);
        let Commands::Invite { opts } = cli.command else {
            panic!("expected Invite command");
        };
        assert!(opts.ttl.is_none());
    }
}
