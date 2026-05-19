//! Anti-entropy: periodic digest broadcast + gap-fill resend. Recovers
//! messages a node missed while partitioned/asleep/just-joined.

use std::collections::HashSet;

use bytes::Bytes;
use iroh_gossip::api::GossipSender;

use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::protocol::{Message, MessageBody, Nickname, SwarmId};
use crate::util::tuning::{ANTIENTROPY_DIGEST_MAX_IDS, ANTIENTROPY_MAX_RESEND};

use super::broadcast_msg;

/// Broadcast an anti-entropy digest: the ids of the recent messages we
/// hold. A peer that receives it re-sends anything we lack, so a node
/// that missed messages while partitioned/asleep/just-joined recovers.
/// Like `PeerInfo`, never logged or surfaced to consumers.
pub(crate) async fn broadcast_digest(
    state: &EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
) {
    // No real peer has ever linked — a digest would broadcast into the
    // void (mirrors `tick_heal`/`tick_alive` no-peer guards).
    if !state.meshed {
        return;
    }
    let ids = state.message_log.recent_ids(ANTIENTROPY_DIGEST_MAX_IDS);
    let Ok(json) = serde_json::to_string(&ids) else {
        return;
    };
    let Ok(body) = MessageBody::new(json) else {
        return;
    };
    tracing::trace!(ids = ids.len(), "anti-entropy digest broadcast");
    broadcast_msg(sender, &Message::new_digest(swarm, author, body)).await;
}

/// Handle a received anti-entropy digest: re-broadcast up to
/// `ANTIENTROPY_MAX_RESEND` of our logged messages the sender lacks.
/// Receivers that already have them drop the repeat (dedup); the
/// sender (and anyone else who missed them) recovers. Never logged.
pub(crate) async fn handle_digest(message: &Message, state: &EventLoopState, ctx: &HandlerCtx<'_>) {
    let Ok(have_ids) = serde_json::from_str::<Vec<String>>(message.body.as_str()) else {
        return;
    };
    let have: HashSet<&str> = have_ids.iter().map(String::as_str).collect();
    let mut resent = 0usize;
    for msg in state
        .message_log
        .missing_from(&have, ANTIENTROPY_MAX_RESEND)
    {
        if let Ok(bytes) = msg.serialize() {
            let _ = ctx.sender.broadcast(Bytes::from(bytes)).await;
            resent += 1;
        }
    }
    if resent > 0 {
        tracing::debug!(
            resent,
            peer_has = have.len(),
            "anti-entropy: resent messages a peer was missing"
        );
    }
}
