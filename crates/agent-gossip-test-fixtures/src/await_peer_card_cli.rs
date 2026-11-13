use std::time::Instant;

use agent_gossip::Channel;

use crate::{MSG_TIMEOUT, POLL, card_slot_of, cli_channel_get};

/// Block until `target` has an entry in the `meta` doc of `nickname`.
///
/// This is the subprocess counterpart of
/// [`InProcNode::await_peer_card`](crate::InProcNode::await_peer_card). The entry shows that the
/// peer joined and wrote its own slice, so the daemon builds the card of the peer from its roster.
///
/// # Panics
///
/// Panics if the entry does not reach the meta doc before `MSG_TIMEOUT`.
pub fn await_peer_card_cli(mesh: &str, nickname: &str, target: &str) {
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let raw = cli_channel_get(Channel::Meta, mesh, nickname);
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
            && card_slot_of(&value["document"], target).is_some()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{target}'s card never reached {nickname}'s meta doc"
        );
        std::thread::sleep(POLL);
    }
}
