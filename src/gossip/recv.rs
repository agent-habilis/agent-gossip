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
            // Later neighbors only get a `PeerInfo` re-send, and only once
            // per cooldown window per endpoint: a flapping link must not
            // re-flood our address to the whole mesh on every up-transition
            // (the residual amplifier behind the soak's `neighbor up` storm).
            let now = Instant::now();
            if state.announced {
                if state.peerinfo_on_cooldown(node_id, now) {
                    tracing::debug!(endpoint_id = %node_id, "skipped PeerInfo re-flood (cooldown)");
                } else {
                    broadcast_peer_info(
                        ctx.sender,
                        ctx.swarm,
                        ctx.author,
                        ctx.identity,
                        ctx.endpoint,
                    )
                    .await;
                    state.note_peerinfo(node_id, now);
                    state.last_sent_at = now;
                }
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
                state.note_peerinfo(node_id, now);
                state.last_sent_at = now;
                tracing::info!("announced arrival on first gossip link");
            }
            // The co-hosted rendezvous is overlay plumbing, not a
            // participant — never cache its pseudo-node in the
            // bootstrap set. Transport links are not surfaced at all:
            // arrival is surfaced once, via membership presence
            // (`joined`), keyed by nickname and join-horizon gated.
            if node_id != ctx.rendezvous_id {
                // `NeighborUp`/`NeighborDown` are the only writers of
                // `linked_endpoints`: it must mirror the *live* overlay
                // links, because the silent-partition WARN and the
                // re-bridge gate read it as link truth. (A `PeerInfo`
                // used to insert here optimistically — before any link
                // formed — leaving permanent ghosts that suppressed
                // both; see the 2026-06-12 roster-collapse review.)
                state.linked_endpoints.insert(node_id);
                // First link to a *real* peer: now (and only now) can
                // user content actually be delivered. Flush anything
                // buffered while we were unmeshed, in order.
                if !state.meshed {
                    state.meshed = true;
                    state.degraded = false;
                    flush_pending(state, ctx, "first real-peer link up").await;
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
            // Upstream closes a lagging subscriber outright (its docs:
            // "close and re-open"), so a terminal `None` follows; the
            // heal arm's resubscribe handles the re-open.
            ctx.output
                .info("Event stream lagged, some messages may have been missed");
            tracing::warn!("gossip event stream lagged; some messages missed");
        }
        Some(Err(error)) => {
            ctx.output.error(&format!("Gossip error: {error}"));
            tracing::warn!(%error, "gossip error");
        }
        None => {
            // Terminal: the actor closed this subscription (lag
            // eviction) or died. The heal arm re-subscribes; meanwhile
            // IPC msg/poll keep serving the local buffer.
            state.gossip_open = false;
            ctx.output.error("gossip stream ended; resubscribing");
            tracing::error!("gossip stream ended; heal arm will resubscribe");
        }
    }
}

/// Drain the message payloads a dead subscription buffered before its
/// stream ended, so they reach the app instead of vanishing. The gossip
/// actor already counts them as delivered — its overlay dedup will
/// *not* re-push them, and anti-entropy resends of them are likewise
/// dropped below the app — so the buffer is the only copy this node
/// will ever see. Only `Received` payloads are processed: neighbor
/// up/down from a dead subscription is stale link state the fresh
/// subscription re-derives immediately.
pub(crate) async fn drain_dead_receiver(
    receiver: &mut iroh_gossip::api::GossipReceiver,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    use futures_util::{FutureExt as _, StreamExt as _};
    let mut recovered = 0usize;
    loop {
        match receiver.next().now_or_never() {
            Some(Some(Ok(Event::Received(incoming)))) => {
                handle_gossip_received(incoming.content, state, ctx).await;
                recovered += 1;
            }
            // Skip stale membership events / errors; stop on a terminal
            // `None` (the stream's actual end) or an empty buffer.
            Some(Some(_)) => {}
            Some(None) | None => break,
        }
    }
    tracing::info!(
        recovered,
        "drained buffered messages from the dead gossip subscription"
    );
}

/// Drain `pending_outbound` onto the wire, in order. Shared by the two
/// meshed edges: the first real-peer `NeighborUp`, and a degraded
/// node's first inbound message after a fault (starvation recovery /
/// resume), where traffic — not a fresh link — is the healthy signal.
async fn flush_pending(state: &mut EventLoopState, ctx: &HandlerCtx<'_>, edge: &'static str) {
    let mut flushed = 0usize;
    for bytes in state.pending_outbound.take() {
        if let Err(error) = ctx.sender.broadcast(bytes).await {
            tracing::warn!(%error, "flush of a buffered outbound message failed");
        }
        flushed += 1;
    }
    tracing::info!(flushed, edge, "meshed: flushed buffered messages");
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
    // only our own signing key identifies our own echoed broadcasts. The hex
    // pubkey is computed once at loop setup (`ctx.our_pubkey`), so this is a
    // string compare, not a key-derivation + allocation per message.
    if message.pubkey == ctx.our_pubkey {
        return;
    }
    tracing::trace!(author = %message.author, "gossip message received");
    // Authenticity gate, **before** dedup: every inbound message must carry a
    // valid signature over its canonical bytes. Verifying before `mark_seen`
    // stops a forged/unsigned message from poisoning the dedup window with a
    // replayed id and suppressing the genuine signed copy. Relayed and
    // anti-entropy copies keep their original author's signature, so they
    // verify here too. Canonical bytes are computed once and reused for the
    // content hash at the log site, so a Msg is not re-serialized twice.
    let canonical = message.canonical_bytes();
    if !message.verify_signature_with(&canonical) {
        tracing::warn!(author = %message.author, "dropping message with missing/invalid signature");
        return;
    }
    // Starvation watchdog signal, *before* dedup: even a duplicate
    // delivery proves the mesh carries traffic. On the degraded→meshed
    // edge (recovery succeeded) the outbound buffer flushes here.
    if state.note_inbound(Instant::now()) {
        flush_pending(state, ctx, "inbound traffic resumed").await;
    }
    // Duplicate suppression: a true repeat delivery must not re-rate-count,
    // re-heartbeat, re-run membership, re-embed-forward, re-log, or re-print.
    // Re-broadcasts of `joined`/`Alive` mint fresh ids so they are never
    // falsely suppressed here. Only authenticated messages reach this gate.
    if state.mark_seen(&message.id) {
        return;
    }
    // Identity is the signing key, not the nickname (p2panda-style): the
    // signature above authenticates the *key*; the `author` nickname is a
    // non-unique display label and is deliberately **not** pinned/claimed,
    // so a nickname is never "burned" by a restart on a long-lived swarm.
    // Identities are distinguished by their key fingerprint, not the name.
    // Fork (equivocation) detection and DAG folding happen at the log-push
    // site below, coupled to retention so their indexes stay bounded by the
    // log window — a rate-dropped or relay-to-other Msg is never indexed.
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
            // the small swarms and rare manual `ah-s ping` this serves.
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

    if is_loggable(&message.kind) {
        // Fork (equivocation) detection + cross-author DAG folding are coupled
        // to logging: only a message we actually **retain** is folded into the
        // indexes, so `by_hash`/`dag_heads`/`author_seqs` stay bounded by the
        // log window (pruned on eviction below). A rate-dropped Msg, or a reply
        // addressed to another peer, returned earlier and never reaches here —
        // so it can no longer leak a permanent index entry. Only `Msg` carries
        // a `seq`/parents; presence is loggable but not indexed.
        if matches!(message.kind, MessageKind::Msg { .. }) {
            let hash = identity::content_hash_hex(&canonical);
            // A second, *different* content hash at an already-seen
            // `(pubkey, seq)` is cryptographic proof of equivocation — surface
            // a `fork` once per offending key (order-independent; we keep both).
            if let Some(seq) = message.seq
                && state.note_msg_seq(&message.pubkey, seq, hash.clone())
            {
                ctx.output.fork(&message.author, &message.pubkey, seq);
                tracing::warn!(author = %message.author, seq, "fork detected: conflicting messages at same seq");
            }
            // Fold into the DAG tip set; flag a message whose timestamp
            // precedes a referenced parent (backdating).
            if state.note_dag(hash, &message.parents, message.timestamp) {
                tracing::warn!(author = %message.author, "message timestamp precedes a referenced parent; possible backdating");
            }
        }
        if let Some(evicted) = state.message_log.push(message) {
            // Keep the DAG + fork indexes bounded with the log window.
            let evicted_hash = evicted.content_hash_hex();
            state.forget_hash(&evicted_hash);
            if let Some(seq) = evicted.seq {
                state.forget_msg_seq(&evicted.pubkey, seq, &evicted_hash);
            }
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
    if peer_id == ctx.endpoint.id() || peer_id == ctx.rendezvous_id {
        return;
    }
    // Remember every advertised peer for the rendezvous-independent
    // re-bridge, and register its address once. Unconditional on link
    // state — a `PeerInfo` normally arrives over an already-formed link,
    // and recovery (`heal::rebridge_known`, the starvation watchdog's
    // precondition) needs the memory precisely *after* that link dies.
    if state.known_endpoints.insert(peer_id) {
        let _ = add_peer_addr(ctx.endpoint, peer_addr.clone());
    }
    let now = Instant::now();
    // A `PeerInfo` is a dial hint, never a link: `linked_endpoints` is
    // owned by `NeighborUp`/`NeighborDown` (link truth), so an unlinked
    // peer's `PeerInfo` only re-registers its (possibly fresh) address
    // and *asks* the gossip actor to graft it. Until the link
    // materializes, each post-cooldown re-receipt retries the dial —
    // the healer's per-peer backstop.
    if state.linked_endpoints.len() < ctx.max_peers
        && !state.linked_endpoints.contains(&peer_id)
        && !state.relink_on_cooldown(peer_id, now)
    {
        state.note_relink(peer_id, now);
        let _ = add_peer_addr(ctx.endpoint, peer_addr);
        if let Err(error) = ctx.sender.join_peers(vec![peer_id]).await {
            tracing::warn!(endpoint_id = %peer_id, %error, "PeerInfo graft request failed");
        }
        let _ = ctx.sender.broadcast(content).await;
        state.last_sent_at = Instant::now();
        tracing::debug!(
            endpoint_id = %peer_id,
            linked = state.linked_endpoints.len(),
            "dialing peer from PeerInfo"
        );
    }
}

/// `Alive` keepalives, `PeerInfo` endpoint plumbing, anti-entropy `Digest`s,
/// and ping/pong probes are infrastructure; everything else (real `Msg`s and
/// `joined`/`left` presence) goes in the log (and so to `poll`/`fetch`).
///
/// `PeerInfo` is classified as non-loggable here so the rule is encoded in one
/// place rather than relying on its match arm's early `return`: a future code
/// path that reaches the log gate must not start persisting endpoint-address
/// plumbing into the poll/fetch buffer.
fn is_loggable(kind: &MessageKind) -> bool {
    !matches!(
        kind,
        MessageKind::Presence {
            subtype: crate::protocol::PresenceSubtype::Alive
        } | MessageKind::PeerInfo
            | MessageKind::Digest
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
    fn msg_is_loggable_but_peerinfo_is_not() {
        assert!(is_loggable(&MessageKind::Msg { reply: None }));
        // PeerInfo is endpoint plumbing — never logged/surfaced, and the
        // classifier (not just its early-return) now says so.
        assert!(!is_loggable(&MessageKind::PeerInfo));
    }
}
