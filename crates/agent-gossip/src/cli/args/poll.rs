//! `poll` command args: retrieve buffered messages from a running gossip
//! process via IPC.

use clap::Parser;

use super::legacy::LegacyOutput;

use agent_habilis_mesh::protocol::{MeshId, Nickname};

#[derive(Parser, Debug)]
pub(crate) struct PollOpts {
    /// Gossip identifier
    #[arg(long, alias = "room", required_unless_present = "state_file")]
    pub gossip: Option<MeshId>,

    /// Nickname of the local agent (must have a running join/create session)
    #[arg(long, required_unless_present = "state_file")]
    pub nickname: Option<Nickname>,

    /// Resolve --gossip/--nickname from a create/join --state-file instead:
    /// wait until that file reports the daemon serving (like `agent-gossip ready`),
    /// then poll as the identity it carries. Lets a poll be armed before the
    /// daemon has minted its identity.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["gossip", "nickname"])]
    pub state_file: Option<std::path::PathBuf>,

    /// Debug/recovery read: only return events surfaced after this sequence
    /// number, without touching the daemon's read cursor. Hidden — a plain
    /// `poll` tracks the cursor for you (it serves everything not yet served
    /// and remembers where you are).
    #[arg(long, hide = true)]
    pub after: Option<u64>,

    /// Block until an unserved event arrives (long-poll) — the receive bell.
    /// Parks until the daemon holds a waking event it has not yet served to a
    /// plain poll; state/meta document echoes never fire it. The daemon holds
    /// each request up to ~60s and the CLI transparently re-issues on an
    /// empty window, so this never times out; a killed call loses nothing.
    /// Omit for an immediate read.
    #[arg(long)]
    pub long: bool,

    #[command(flatten)]
    pub legacy_output: LegacyOutput,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    fn parse(argv: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(argv)
    }

    #[test]
    fn gossip_and_nickname_form_parses() {
        let cli = parse(&[
            "agent-gossip",
            "poll",
            "--gossip",
            "abc",
            "--nickname",
            "calm-fox",
        ])
        .expect("gossip+nickname form must parse");
        let Commands::Poll { opts } = cli.command else {
            panic!("expected poll");
        };
        assert!(opts.gossip.is_some());
        assert!(opts.nickname.is_some());
        assert!(opts.state_file.is_none());
    }

    #[test]
    fn state_file_form_parses_without_identity() {
        let cli = parse(&[
            "agent-gossip",
            "poll",
            "--state-file",
            "/tmp/s.json",
            "--long",
        ])
        .expect("state-file form must parse");
        let Commands::Poll { opts } = cli.command else {
            panic!("expected poll");
        };
        assert!(opts.gossip.is_none());
        assert!(opts.nickname.is_none());
        assert!(opts.state_file.is_some());
        assert!(opts.long);
    }

    #[test]
    fn state_file_conflicts_with_gossip_and_nickname() {
        assert!(
            parse(&[
                "agent-gossip",
                "poll",
                "--state-file",
                "/tmp/s.json",
                "--gossip",
                "abc",
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "agent-gossip",
                "poll",
                "--state-file",
                "/tmp/s.json",
                "--nickname",
                "calm-fox",
            ])
            .is_err()
        );
    }

    /// `--after` is hidden from help but must keep parsing — it is the
    /// debug/recovery replay and the old-protocol compatibility path.
    #[test]
    fn hidden_after_still_parses() {
        let cli = parse(&[
            "agent-gossip",
            "poll",
            "--gossip",
            "abc",
            "--nickname",
            "calm-fox",
            "--after",
            "41",
        ])
        .expect("hidden flag parses");
        let Commands::Poll { opts } = cli.command else {
            panic!("expected poll");
        };
        assert_eq!(opts.after, Some(41));
    }

    #[test]
    fn identity_is_required_without_state_file() {
        assert!(parse(&["agent-gossip", "poll"]).is_err());
        assert!(parse(&["agent-gossip", "poll", "--gossip", "abc"]).is_err());
        assert!(parse(&["agent-gossip", "poll", "--nickname", "calm-fox"]).is_err());
    }
}
