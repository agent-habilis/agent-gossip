//! Gossip **inbound plane**: the gossip-event pump (`Received` /
//! `NeighborUp` / `NeighborDown` / `Lagged` / stream end), neighbor
//! up/down bookkeeping, the per-message router (parse → self-echo drop →
//! rate-check → lifecycle observe → dispatch by kind → message-log), and
//! `PeerInfo` linking. Outbound/send lives in [`super::broadcast`]; this
//! layer never touches the participant roster directly — it calls into
//! `lifecycle::observe` and dispatches by kind.

use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh_gossip::api::{ApiError, Event};

use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::lifecycle;
use crate::lookup::add_peer_addr;
use crate::protocol::identity;
use crate::protocol::{Message, MessageKind};
use crate::util::tuning::RECLAIM_WINDOW_SECS;

use super::broadcast::{announce_arrival, broadcast_msg, broadcast_peer_info};
use super::{antientropy, conn_path};

/// Dispatch a single item from the gossip receiver stream:
/// `Received` → `handle_gossip_received`; `NeighborUp` →
/// announce / `PeerInfo` re-send; `NeighborDown` → prune the link and
/// arm reclaim; `Lagged` / errors logged; terminal `None` flips
/// `state.gossip_open` to `false`.
pub(crate) async fn handle_gossip_event(
    event: Option<Result<Event, ApiError>>,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    match event {
        Some(Ok(Event::Received(received))) => {
            handle_gossip_received(received.content, state, ctx).await;
        }
        Some(Ok(Event::NeighborUp(node_id))) => {
            let (conn, relay) = conn_path(ctx.endpoint, node_id).await;
            tracing::info!(
                endpoint_id = %node_id,
                is_rendezvous = node_id == ctx.rendezvous_id,
                conn,
                relay = relay.as_ref().map_or("-", |url| url.as_str()),
                "gossip neighbor up"
            );
            // Announce on the first link, not at startup: a joiner's
            // first link is the rendezvous relay, and `joined` is a
            // one-shot — sending it before any link exists loses it.
            // Later neighbors only get a `PeerInfo` re-send.
            if state.announced {
                broadcast_peer_info(
                    ctx.sender,
                    ctx.swarm,
                    ctx.author,
                    ctx.identity,
                    ctx.endpoint,
                )
                .await;
            } else {
                announce_arrival(
                    ctx.sender,
                    ctx.swarm,
                    ctx.author,
                    ctx.identity,
                    ctx.endpoint,
                )
                .await;
                state.announced = true;
                tracing::info!("announced arrival on first gossip link");
            }
            state.last_sent_at = Instant::now();
            // The co-hosted rendezvous is overlay plumbing, not a
            // participant — never cache its pseudo-node in the
            // bootstrap set. Transport links are not surfaced at all:
            // arrival is surfaced once, via membership presence
            // (`joined`), keyed by nickname and join-horizon gated.
            if node_id != ctx.rendezvous_id {
                // First link to a *real* peer: now (and only now) can
                // user content actually be delivered. Flush anything
                // buffered while we were unmeshed, in order.
                if !state.meshed {
                    state.meshed = true;
                    let mut flushed = 0usize;
                    for bytes in state.pending_outbound.take() {
                        let _ = ctx.sender.broadcast(bytes).await;
                        flushed += 1;
                    }
                    tracing::info!(
                        flushed,
                        "meshed: first real-peer link up, flushed buffered messages"
                    );
                }
            }
        }
        Some(Ok(Event::NeighborDown(node_id))) => {
            let is_rendezvous = node_id == ctx.rendezvous_id;
            tracing::info!(endpoint_id = %node_id, is_rendezvous, "gossip neighbor down");
            if !is_rendezvous {
                state.linked_endpoints.remove(&node_id);
            }
            // Arm the fast reclaim burst only on a real beacon-loss /
            // isolation signal: the rendezvous link dropped, or that
            // was our last tracked peer. A plain HyParView shuffle (a
            // participant flapping while others remain) must NOT arm
            // it, or initial multi-node convergence pays a needless
            // ~6s bind storm on every non-beacon node.
            if is_rendezvous || state.linked_endpoints.is_empty() {
                state.reclaim_until =
                    Some(Instant::now() + Duration::from_secs(RECLAIM_WINDOW_SECS));
                tracing::info!(
                    reason = if is_rendezvous {
                        "rendezvous-loss"
                    } else {
                        "last-peer"
                    },
                    "armed fast reclaim window"
                );
            }
        }
        Some(Ok(Event::Lagged)) => {
            ctx.output
                .info("Event stream lagged, some messages may have been missed");
            tracing::warn!("gossip event stream lagged; some messages missed");
        }
        Some(Err(error)) => {
            ctx.output.error(&format!("Gossip error: {error}"));
            tracing::warn!(%error, "gossip error");
        }
        None => {
            // Stream ended. IPC keeps working for msg/poll.
            state.gossip_open = false;
            tracing::warn!("gossip stream ended; IPC msg/poll still works");
        }
    }
}

/// Handle a single received gossip payload: parse, drop self-echo,
/// rate-check, run the lifecycle observer (heartbeat / membership /
/// surfacing / horizon), dispatch by kind, and finally push to the
/// message log if loggable.
async fn handle_gossip_received(content: Bytes, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    let Ok(message) = Message::parse(&content) else {
        ctx.output.error("Failed to parse message");
        tracing::warn!("failed to parse inbound gossip message");
        return;
    };
    // Self-echo drop: keyed on our **public key**, not the nickname. With
    // non-unique display names a peer may legitimately share our nickname —
    // only our own signing key identifies our own echoed broadcasts.
    if message.pubkey == identity::encode_pubkey(&ctx.identity.public()) {
        return;
    }
    tracing::trace!(author = %message.author, "gossip message received");
    // Duplicate suppression: a true repeat delivery must not
    // re-rate-count, re-heartbeat, re-run membership, re-embed-forward,
    // re-log, or re-print. Re-broadcasts of `joined`/`Alive` mint fresh
    // ids so they are never falsely suppressed here.
    if state.mark_seen(&message.id) {
        return;
    }
    // Authenticity gate: every inbound message must carry a valid
    // signature over its canonical bytes. Drop unsigned / tampered /
    // wrong-key messages before they are rate-counted, surfaced, logged,
    // or acted on. Relayed and anti-entropy copies keep their original
    // author's signature, so they verify here too.
    if !message.verify_signature() {
        tracing::warn!(author = %message.author, "dropping message with missing/invalid signature");
        return;
    }
    // Identity is the signing key, not the nickname (p2panda-style): the
    // signature above authenticates the *key*; the `author` nickname is a
    // non-unique display label and is deliberately **not** pinned/claimed,
    // so a nickname is never "burned" by a restart on a long-lived swarm.
    // Identities are distinguished by their key fingerprint, not the name.
    //
    // Phase 2 fork (equivocation) detection: only `Msg` carries `seq`. A
    // second, *different* content hash at an already-seen `(pubkey, seq)` is
    // cryptographic proof the author signed conflicting messages — surface a
    // `fork` once per offending key. Order-independent (gossip is unordered);
    // the message itself is still processed (we keep both, never auto-pick).
    if matches!(message.kind, MessageKind::Msg { .. }) {
        let hash = message.content_hash_hex();
        // Per-author fork (equivocation) detection — Msgs that carry a seq.
        if let Some(seq) = message.seq
            && state.note_msg_seq(&message.pubkey, seq, hash.clone())
        {
            ctx.output.fork(&message.author, &message.pubkey, seq);
            tracing::warn!(author = %message.author, seq, "fork detected: conflicting messages at same seq");
        }
        // Cross-author DAG (Phase 3): fold into the tip set; flag a message
        // whose timestamp precedes a referenced parent (backdating).
        if state.note_dag(hash, &message.parents, message.timestamp) {
            tracing::warn!(author = %message.author, "message timestamp precedes a referenced parent; possible backdating");
        }
    }
    // One quota for every Msg (open or reply); plumbing kinds (presence,
    // Alive, digest, PeerInfo) are exempt — rate-limiting them would
    // break membership/anti-entropy. Keyed on the verified pubkey.
    let rate_ok = match &message.kind {
        MessageKind::Msg { .. } => state.rate_limiter.check(&message.pubkey),
        MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::Ping
        | MessageKind::Pong { .. } => true,
    };
    if !rate_ok {
        let notice = format!("rate limit exceeded for [{}], dropping", message.author);
        ctx.output.info(&notice);
        tracing::debug!(author = %message.author, "rate limit exceeded; dropping");
        return;
    }

    // Heartbeat + membership + surfacing + join horizon. The lifecycle
    // layer owns every roster/presentation side effect; the gossip
    // layer only routes by kind below.
    crate::logging::messages::log_in(&message);
    let observed = lifecycle::observe(&message, state, ctx);
    let surfaceable = observed.surfaceable;

    // Embed push: hand every surviving inbound message to the facade
    // before kind routing, so the consumer sees msg / presence /
    // peer_info alike. Non-blocking by construction (bounded
    // broadcast); a send error or full ring is intentionally dropped
    // so a slow embedder never stalls the gossip loop. The
    // receiver_count gate skips the per-message clone while no
    // consumer is subscribed (always, until the embedder calls
    // messages(); forever for CLI/MCP where the field is None).
    // `surfaceable` keeps pre-join backlog off the embed channel too.
    if let Some(tx) = ctx.external_msg_tx
        && tx.receiver_count() > 0
        && surfaceable
        && !matches!(
            message.kind,
            MessageKind::Digest | MessageKind::Ping | MessageKind::Pong { .. }
        )
    {
        let _ = tx.send(message.clone());
    }

    match &message.kind {
        MessageKind::PeerInfo => {
            handle_peer_info(&message, content, state, ctx).await;
            return;
        }
        MessageKind::Digest => {
            antientropy::handle_digest(&message, state, ctx).await;
            return;
        }
        MessageKind::Ping => {
            // Auto-respond to every probe with a pong addressed to the
            // pinger. The daemon owns this — no agent involvement. Pong
            // is gossip-broadcast (no unicast transport), so one probe in
            // an N-node swarm fans out to N flooded pongs; acceptable for
            // the small swarms and rare manual `ahs ping` this serves.
            broadcast_msg(
                ctx.sender,
                &Message::new_pong(ctx.swarm, ctx.author, message.author.clone())
                    .signed(ctx.identity),
            )
            .await;
            return;
        }
        MessageKind::Pong { to } => {
            // Record arrival for the active round only if addressed to us
            // and from a known participant — the roster gate bounds the
            // map and keeps `responded`/`known` honest against a peer that
            // forges pongs from fabricated authors.
            if to == ctx.author
                && state.participants.contains(message.author.as_str())
                && let Some(round) = state.ping_round.as_mut()
            {
                round
                    .pongs
                    .insert(message.author.clone(), tokio::time::Instant::now());
            }
            return;
        }
        MessageKind::Presence { subtype } => {
            lifecycle::handle_presence(
                &message,
                *subtype,
                &observed.update,
                surfaceable,
                state,
                ctx,
            )
            .await;
        }
        MessageKind::Msg { .. } => {
            if !lifecycle::handle_msg(ctx.output, &message, surfaceable, ctx.author) {
                return;
            }
        }
    }

    if is_loggable(&message.kind)
        && let Some(evicted) = state.message_log.push(message)
    {
        // Keep the DAG + fork indexes bounded with the log window.
        state.forget_hash(&evicted.content_hash_hex());
        if let Some(seq) = evicted.seq {
            state.forget_msg_seq(&evicted.pubkey, seq);
        }
    }
}

async fn handle_peer_info(
    message: &Message,
    content: Bytes,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(message.body.as_str()) else {
        return;
    };
    let Ok((peer_id, peer_addr)) = crate::protocol::peer_addr::endpoint_addr_from_json(&parsed)
    else {
        return;
    };
    let now = Instant::now();
    if peer_id != ctx.endpoint.id()
        && peer_id != ctx.rendezvous_id
        && state.linked_endpoints.len() < ctx.max_peers
        && !state.relink_on_cooldown(peer_id, now)
        && state.linked_endpoints.insert(peer_id)
    {
        state.note_relink(peer_id, now);
        let _ = add_peer_addr(ctx.endpoint, peer_addr);
        // Remember this peer for the rendezvous-independent re-bridge: it
        // survives a later `NeighborDown`, and iroh keeps the address we
        // just added, so the healer can re-dial it directly if the
        // rendezvous/relay goes unreachable (see `heal::rebridge_known`).
        state.known_endpoints.insert(peer_id);
        let _ = ctx.sender.join_peers(vec![peer_id]).await;
        let _ = ctx.sender.broadcast(content).await;
        state.last_sent_at = Instant::now();
        tracing::debug!(
            endpoint_id = %peer_id,
            linked = state.linked_endpoints.len(),
            "linked new peer from PeerInfo"
        );
    }
}

/// `Alive` keepalives, anti-entropy `Digest`s, and ping/pong probes are
/// plumbing; everything else goes in the log (and so to `poll`/`fetch`).
fn is_loggable(kind: &MessageKind) -> bool {
    !matches!(
        kind,
        MessageKind::Presence {
            subtype: crate::protocol::PresenceSubtype::Alive
        } | MessageKind::Digest
            | MessageKind::Ping
            | MessageKind::Pong { .. }
    )
}

#[cfg(test)]
mod is_loggable_tests {
    use super::is_loggable;
    use crate::protocol::{MessageKind, PresenceSubtype};

    #[test]
    fn alive_presence_is_not_loggable() {
        assert!(!is_loggable(&MessageKind::Presence {
            subtype: PresenceSubtype::Alive
        }));
    }

    #[test]
    fn joined_and_left_presence_are_loggable() {
        assert!(is_loggable(&MessageKind::Presence {
            subtype: PresenceSubtype::Joined
        }));
        assert!(is_loggable(&MessageKind::Presence {
            subtype: PresenceSubtype::Left
        }));
    }

    #[test]
    fn msg_peerinfo_are_loggable() {
        assert!(is_loggable(&MessageKind::Msg { reply: None }));
        assert!(is_loggable(&MessageKind::PeerInfo));
    }
}
