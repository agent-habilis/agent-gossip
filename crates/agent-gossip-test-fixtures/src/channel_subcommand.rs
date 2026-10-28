use agent_gossip::Channel;

/// The CLI subcommand for a channel (`state` / `meta`) — `Channel::label` is
/// `pub(crate)`, not reachable from outside `agent-gossip`.
pub fn channel_subcommand(channel: Channel) -> &'static str {
    match channel {
        Channel::State => "state",
        Channel::Meta => "meta",
        _ => panic!("the fixture has no subcommand for this channel"),
    }
}
