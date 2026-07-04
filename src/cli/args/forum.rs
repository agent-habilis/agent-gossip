use clap::Parser;

use crate::protocol::Nickname;

use super::shared::SharedServerOpts;

/// Reject an empty / whitespace-only forum string at parse time — it would
/// otherwise hash to a real (but useless, un-guessable-on-purpose) swarm.
fn non_empty_string(raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("forum string must not be empty".to_owned());
    }
    Ok(raw.to_owned())
}

#[derive(Parser, Debug)]
pub(crate) struct ForumOpts {
    /// Any string — hashed into a deterministic **public** swarm. The same
    /// string always joins the same forum, on any machine, with no id to
    /// share. Compared byte-for-byte after trimming surrounding whitespace, so
    /// `http://…` and `https://…`, or `Repo` and `repo`, are different forums.
    // allow_hyphen_values: "any string" includes ones starting with `-`
    // (e.g. `-release-2026`); without it clap rejects them as unknown flags.
    #[arg(value_parser = non_empty_string, allow_hyphen_values = true)]
    pub string: String,

    /// Optional nickname (random word-word if not provided). A custom
    /// nickname is 1..=32 UTF-8 characters, excluding control chars,
    /// whitespace, and any of / \ < > #.
    #[arg(long)]
    pub nickname: Option<Nickname>,

    #[command(flatten)]
    pub shared: SharedServerOpts,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::args::{Cli, Commands};

    fn forum_opts(args: &[&str]) -> super::ForumOpts {
        match Cli::parse_from(args).command {
            Commands::Forum { opts } => opts,
            Commands::Create { .. }
            | Commands::Join { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Peers { .. }
            | Commands::Pipe { .. }
            | Commands::Port { .. }
            | Commands::File { .. }
            | Commands::Sh { .. }
            | Commands::Mount { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Card { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Session { .. } => panic!("expected Forum command"),
        }
    }

    #[test]
    fn forum_parses_string_and_nickname() {
        let opts = forum_opts(&["ahsw", "forum", "agent-habilis", "--nickname", "me"]);
        assert_eq!(opts.string, "agent-habilis");
        assert_eq!(
            opts.nickname
                .as_ref()
                .map(crate::protocol::Nickname::as_str),
            Some("me")
        );
    }

    #[test]
    fn forum_accepts_leading_hyphen_string() {
        let opts = forum_opts(&["ahsw", "forum", "-release-2026"]);
        assert_eq!(opts.string, "-release-2026");
    }

    // The pi extension builds `forum <flags> -- <string>` so a string that
    // collides with a known flag still lands in the positional.
    #[test]
    fn forum_accepts_string_after_end_of_flags() {
        let opts = forum_opts(&["ahsw", "forum", "--nickname", "me", "--", "--nickname"]);
        assert_eq!(opts.string, "--nickname");
    }

    #[test]
    fn forum_string_is_required() {
        assert!(Cli::try_parse_from(["ahsw", "forum"]).is_err());
    }

    #[test]
    fn forum_rejects_empty_string() {
        assert!(Cli::try_parse_from(["ahsw", "forum", ""]).is_err());
        assert!(
            Cli::try_parse_from(["ahsw", "forum", "   "]).is_err(),
            "whitespace-only must reject"
        );
    }
}
