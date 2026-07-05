use clap::Parser;

use crate::protocol::Nickname;

use super::shared::SharedServerOpts;

/// Reject an empty / whitespace-only topic string at parse time — it would
/// otherwise hash to a real (but useless, un-guessable-on-purpose) swarm.
fn non_empty_string(raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("topic string must not be empty".to_owned());
    }
    Ok(raw.to_owned())
}

#[derive(Parser, Debug)]
pub(crate) struct TopicOpts {
    /// Any string — hashed into a deterministic **public** swarm. The same
    /// string always joins the same topic, on any machine, with no id to
    /// share. Compared byte-for-byte after trimming surrounding whitespace, so
    /// `http://…` and `https://…`, or `Repo` and `repo`, are different topics.
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

    fn topic_opts(args: &[&str]) -> super::TopicOpts {
        match Cli::parse_from(args).command {
            Commands::Topic { opts } => opts,
            Commands::Create { .. }
            | Commands::Join { .. }
            | Commands::Poll { .. }
            | Commands::Ping { .. }
            | Commands::Discover { .. }
            | Commands::Peers { .. }
            | Commands::State { .. }
            | Commands::Meta { .. }
            | Commands::Topology { .. }
            | Commands::A2a { .. }
            | Commands::Ready { .. }
            | Commands::Mcp { .. }
            | Commands::Man
            | Commands::Plug { .. }
            | Commands::Unplug { .. }
            | Commands::Doctor { .. }
            | Commands::Leave { .. }
            | Commands::Invite { .. }
            | Commands::Session { .. } => panic!("expected Topic command"),
        }
    }

    #[test]
    fn topic_parses_string_and_nickname() {
        let opts = topic_opts(&["agent-gossip", "topic", "agent-habilis", "--nickname", "me"]);
        assert_eq!(opts.string, "agent-habilis");
        assert_eq!(
            opts.nickname
                .as_ref()
                .map(crate::protocol::Nickname::as_str),
            Some("me")
        );
    }

    #[test]
    fn topic_accepts_leading_hyphen_string() {
        let opts = topic_opts(&["agent-gossip", "topic", "-release-2026"]);
        assert_eq!(opts.string, "-release-2026");
    }

    // The pi extension builds `topic <flags> -- <string>` so a string that
    // collides with a known flag still lands in the positional.
    #[test]
    fn topic_accepts_string_after_end_of_flags() {
        let opts = topic_opts(&[
            "agent-gossip",
            "topic",
            "--nickname",
            "me",
            "--",
            "--nickname",
        ]);
        assert_eq!(opts.string, "--nickname");
    }

    #[test]
    fn topic_string_is_required() {
        assert!(Cli::try_parse_from(["agent-gossip", "topic"]).is_err());
    }

    #[test]
    fn topic_rejects_empty_string() {
        assert!(Cli::try_parse_from(["agent-gossip", "topic", ""]).is_err());
        assert!(
            Cli::try_parse_from(["agent-gossip", "topic", "   "]).is_err(),
            "whitespace-only must reject"
        );
    }
}
