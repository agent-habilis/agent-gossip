use std::time::Instant;

use agent_gossip::Channel;

use crate::{MSG_TIMEOUT, POLL, cli_channel_get};

/// Block until `target`'s card is present in `nickname`'s `meta` doc — the real
/// precondition for any **directed** A2A call, and the subprocess counterpart of
/// [`InProcNode::await_peer_card`].
///
/// Gossip linkage is *not* this signal. A directed frame is sealed to the
/// recipient's published X25519 key (`a2a::card::peer_seal_key`) and is never
/// sent in plaintext, so the daemon rejects the call outright until that key
/// arrives. The key rides the `meta` document, which replicates on its own
/// schedule — independently of the broadcast path a linkage probe observes. A
/// call issued on linkage alone therefore races card propagation and fails with
/// "its encryption key is not known yet (cards still propagating)".
pub fn await_peer_card_cli(mesh: &str, nickname: &str, target: &str) {
    let pointer = format!("/peers/{target}/card");
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let raw = cli_channel_get(Channel::Meta, mesh, nickname);
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
            && value["document"].pointer(&pointer).is_some()
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
