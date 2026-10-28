use agent_gossip::Channel;

/// The CLI subcommand for a channel (`state` / `meta`) — `Channel::label` is
/// `pub(crate)`, not reachable from outside `agent-gossip`.
pub fn channel_subcommand(channel: Channel) -> &'static str {
    match channel {
        Channel::State => "state",
        Channel::Meta => "meta",
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "foreign non_exhaustive enum: the compiler needs a wildcard arm; the named variants are listed before it"
        )]
        _ => panic!("the fixture has no subcommand for this channel"),
    }
}
