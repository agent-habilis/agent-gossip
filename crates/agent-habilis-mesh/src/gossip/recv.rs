//! Gossip **inbound plane**: the gossip-event pump (`Received` /
//! `NeighborUp` / `NeighborDown` / `Lagged` / stream end), neighbor
//! up/down bookkeeping, the per-message router (parse → self-echo drop →
//! rate-check → lifecycle observe → dispatch by kind → message-log), and
//! `PeerInfo` linking. Outbound/send lives in [`super::broadcast`]; this
//! layer never touches the peer roster directly — it calls into
//! `lifecycle::observe` and dispatches by kind.

use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh_gossip::api::{ApiError, Event};

use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::gossip::event::NodeEvent;
use crate::lifecycle;
use crate::lookup::add_peer_addr;
use crate::protocol::identity;
use crate::protocol::message::MessageBody;
use crate::protocol::{Channel, Message, MessageKind, Nickname};
use crate::util::tuning::RECLAIM_WINDOW_SECS;

use super::app::{AppClass, InboundApp, NodeApp};
use super::broadcast::{announce_arrival, broadcast_peer_info};
use super::{antientropy, conn_path};

/// Dispatch a single item from the gossip receiver stream:
/// `Received` → `ingest`; `NeighborUp` →
/// announce / `PeerInfo` re-send; `NeighborDown` → prune the link and
/// arm reclaim; `Lagged` / errors logged; terminal `None` flips
/// `state.gossip_open` to `false`.
pub(crate) async fn handle_gossip_event(
    event: Option<Result<Event, ApiError>>,
    state: &mut EventLoopState,
    app: &mut dyn NodeApp,
    ctx: &HandlerCtx<'_>,
) {
    match event {
        Some(Ok(Event::Received(received))) => {
            ingest(received.content, state, app, ctx).await;
        }
        Some(Ok(Event::NeighborUp(node_id))) => {
            let (conn, relay) = conn_path(ctx.endpoint, node_id).await;
            tracing::info!(target: "agent_habilis_mesh::gossip",
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
                    tracing::debug!(target: "agent_habilis_mesh::gossip", endpoint_id = %node_id, "skipped PeerInfo re-flood (cooldown)");
                } else {
                    broadcast_peer_info(ctx).await;
                    state.note_peerinfo(node_id, now);
                    state.last_sent_at = now;
                }
            } else {
                announce_arrival(ctx).await;
                state.announced = true;
                state.note_peerinfo(node_id, now);
                state.last_sent_at = now;
                tracing::info!(target: "agent_habilis_mesh::gossip", "announced arrival on first gossip link");
            }
            // The co-hosted rendezvous is overlay plumbing, not a
            // peer — never cache its pseudo-node in the
            // bootstrap set. Transport links are not surfaced at all:
            // arrival is surfaced once, via membership presence
            // (`joined`), keyed by nickname and join-horizon gated.
            if node_id == ctx.rendezvous_id {
                // Re-arms the healer's probe gate: while this is true the
                // heal tick must not connect-probe the rendezvous (the
                // probe would supersede this very link on the beacon).
                state.rendezvous_linked = true;
            } else {
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
                    // Re-publish anything whose value depends on being meshed
                    // (the app's card dial hint); see `NodeApp::on_meshed`.
                    app.on_meshed(state, ctx).await;
                }
            }
        }
        Some(Ok(Event::NeighborDown(node_id))) => {
            let is_rendezvous = node_id == ctx.rendezvous_id;
            tracing::info!(target: "agent_habilis_mesh::gossip", endpoint_id = %node_id, is_rendezvous, "gossip neighbor down");
            if is_rendezvous {
                state.rendezvous_linked = false;
            } else {
                state.linked_endpoints.remove(&node_id);
            }
            // Arm the fast reclaim burst only on a real beacon-loss /
            // isolation signal: the rendezvous link dropped, or that
            // was our last tracked peer. A plain HyParView shuffle (a
            // peer flapping while others remain) must NOT arm
            // it, or initial multi-node convergence pays a needless
            // ~6s bind storm on every non-beacon node.
            if is_rendezvous || state.linked_endpoints.is_empty() {
                state.reclaim_until =
                    Some(Instant::now() + Duration::from_secs(RECLAIM_WINDOW_SECS));
                tracing::info!(target: "agent_habilis_mesh::gossip",
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
            ctx.sink.emit(NodeEvent::Info(
                "Event stream lagged, some messages may have been missed".to_owned(),
            ));
            tracing::warn!(target: "agent_habilis_mesh::gossip", "gossip event stream lagged; some messages missed");
        }
        Some(Err(error)) => {
            ctx.sink
                .emit(NodeEvent::Error(format!("Gossip error: {error}")));
            tracing::warn!(target: "agent_habilis_mesh::gossip", %error, "gossip error");
        }
        None => {
            // Terminal: the actor closed this subscription (lag
            // eviction) or died. The heal arm re-subscribes; meanwhile
            // IPC msg/poll keep serving the local buffer.
            state.gossip_open = false;
            ctx.sink.emit(NodeEvent::Error(
                "gossip stream ended; resubscribing".to_owned(),
            ));
            tracing::error!(target: "agent_habilis_mesh::gossip", "gossip stream ended; heal arm will resubscribe");
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
    app: &mut dyn NodeApp,
    ctx: &HandlerCtx<'_>,
) {
    use futures_util::{FutureExt as _, StreamExt as _};
    let mut recovered = 0usize;
    loop {
        match receiver.next().now_or_never() {
            Some(Some(Ok(Event::Received(incoming)))) => {
                ingest(incoming.content, state, app, ctx).await;
                recovered += 1;
            }
            // Skip stale membership events / errors; stop on a terminal
            // `None` (the stream's actual end) or an empty buffer.
            Some(Some(_)) => {}
            Some(None) | None => break,
        }
    }
    tracing::info!(target: "agent_habilis_mesh::gossip",
        recovered,
        "drained buffered messages from the dead gossip subscription"
    );
}

/// Drain `pending_outbound` onto the wire, in order. Runs on the meshed
/// edges — the first real-peer `NeighborUp`, and a degraded node's first
/// inbound message after a fault (starvation recovery / resume) — and again
/// on a later `PeerInfo` arrival while frames remain buffered: a frame that
/// fails to deliver (typically a directed frame whose addressee's endpoint
/// isn't known yet at the first flush) is re-buffered for that retry, not
/// dropped.
async fn flush_pending(state: &mut EventLoopState, ctx: &HandlerCtx<'_>, edge: &'static str) {
    let mut delivered = 0usize;
    let mut requeued = 0usize;
    for (msg, bytes) in state.pending_outbound.take() {
        // Re-route each frame through the send decision rather than blanket-
        // broadcasting: a directed frame buffered while unmeshed must still
        // take unicast, never the gossip flood.
        let Err(error) = crate::transport::deliver(&msg, bytes.clone(), state, ctx.sender).await
        else {
            delivered += 1;
            continue;
        };
        if state.pending_outbound.push((msg, bytes)) {
            requeued += 1;
            tracing::debug!(target: "agent_habilis_mesh::gossip", %error, "buffered outbound message not deliverable yet; requeued");
        } else {
            tracing::warn!(target: "agent_habilis_mesh::gossip", %error, "buffered outbound message undeliverable and the buffer is full; dropped");
        }
    }
    tracing::info!(target: "agent_habilis_mesh::gossip",
        delivered,
        requeued,
        edge,
        "meshed: flushed buffered messages"
    );
}

/// Validate + dispatch one inbound wire message, regardless of transport. Both
/// the gossip event pump (`handle_gossip_event`) and the unicast inbound arm
/// (`daemon::event_loop`) funnel here, so signature-verify, the mesh gate, and
/// `mark_seen` dedup are identical on both planes — a message delivered over
/// both surfaces exactly once. `message` is mutable so a directed frame sealed
/// to us can be decrypted in place before surfacing (see `gate_and_decrypt`).
#[expect(
    clippy::too_many_lines,
    reason = "one straight-line validate/dispatch pipeline (parse, self-echo drop, rate-check, lifecycle observe, dispatch by kind, message-log); splitting would scatter a single sequential gate chain across helpers"
)]
pub(crate) async fn ingest(
    content: Bytes,
    state: &mut EventLoopState,
    app: &mut dyn NodeApp,
    ctx: &HandlerCtx<'_>,
) {
    let Ok(mut message) = Message::parse(&content) else {
        ctx.sink
            .emit(NodeEvent::Error("Failed to parse message".to_owned()));
        tracing::warn!(target: "agent_habilis_mesh::gossip", "failed to parse inbound gossip message");
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
    tracing::trace!(target: "agent_habilis_mesh::gossip", author = %message.author, "gossip message received");
    // Authenticity gate, **before** dedup: every inbound message must carry a
    // valid signature over its canonical bytes. Verifying before `mark_seen`
    // stops a forged/unsigned message from poisoning the dedup window with a
    // replayed id and suppressing the genuine signed copy. Relayed and
    // anti-entropy copies keep their original author's signature, so they
    // verify here too. Canonical bytes are computed once and reused for the
    // content hash at the log site, so a Msg is not re-serialized twice.
    let canonical = message.canonical_bytes();
    if !message.verify_signature_with(&canonical) {
        tracing::warn!(target: "agent_habilis_mesh::gossip", author = %message.author, "dropping message with missing/invalid signature");
        return;
    }
    // Mesh gate: the gossip topic already isolates honest meshes, but a peer
    // crafted onto our topic could stamp an arbitrary (signed) `mesh` string
    // that would otherwise flow unchecked into the `--output json` agent API.
    // Drop anything not stamped with our own mesh id.
    if message.mesh != *ctx.mesh {
        tracing::warn!(target: "agent_habilis_mesh::gossip", author = %message.author, "dropping message stamped with a foreign mesh id");
        return;
    }
    // Starvation watchdog signal, *before* dedup: even a duplicate
    // delivery proves the mesh carries traffic. On the degraded→meshed
    // edge (recovery succeeded) the outbound buffer flushes here.
    if state.note_inbound(Instant::now()) {
        flush_pending(state, ctx, "inbound traffic resumed").await;
    }
    // Duplicate suppression: a true repeat delivery must not
    // re-heartbeat, re-run membership, re-push, re-log, or re-print.
    // Keyed on the author-bound dedup key (`SHA-256(pubkey ‖ id)`), so a peer
    // signing its own message under a **victim's** id gets a different key and
    // cannot suppress the victim's genuine message. Re-broadcasts of
    // `joined`/`Alive` mint fresh ids so they are never falsely suppressed
    // here. Only authenticated messages reach this gate.
    if state.mark_seen(&message) {
        return;
    }
    // Identity is the signing key, not the nickname (p2panda-style): the
    // signature above authenticates the *key*; the `author` nickname is a
    // non-unique display label and is deliberately **not** pinned/claimed,
    // so a nickname is never "burned" by a restart on a long-lived mesh.
    // Identities are distinguished by their key fingerprint, not the name.
    // Fork (equivocation) detection and DAG folding happen at the log-push
    // site below, coupled to retention so their indexes stay bounded by the
    // log window.

    // Heartbeat + membership + surfacing + join horizon. The lifecycle
    // layer owns every roster/presentation side effect; the gossip
    // layer only routes by kind below.
    crate::logging::messages::log_in(&message);
    let observed = lifecycle::observe(&message, state, ctx);
    let surfaceable = observed.surfaceable;

    // A shard of a split body never surfaces as a raw slice — see
    // `handle_shard` for the retain/reassemble/gate/surface sequence.
    if message.shard.is_some() {
        handle_shard(
            InboundFrame {
                message: &message,
                canonical: &canonical,
                surfaceable,
            },
            state,
            app,
            ctx,
        )
        .await;
        return;
    }

    // Directed frames are sealed to their addressee. A **relay** forwards and
    // retains the opaque frame but never validates or surfaces its content (it
    // can't read it, and `maybe_push_inbound`/dispatch below are already gated to
    // the addressee). The **addressee** decrypts the body in place, then the
    // recovered plaintext passes the app boundary gate exactly as a broadcast
    // does. A frame sealed to us that fails to open (tampered / wrong key) is
    // dropped.
    // Classify the (now-decrypted) App payload **once** here — inside the gate,
    // where the boundary check already parses it — and thread the result to the
    // push and retain steps so neither re-parses the body. `None` for an infra
    // frame (no app payload).
    let (sealed_body, app_class) = match gate_and_decrypt(&mut message, state, ctx, &*app) {
        Gated::Pass { sealed, class } => (sealed, class),
        Gated::Drop => return,
    };

    maybe_push_inbound(ctx, &message, surfaceable, app_class.as_ref());

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
            // Auto-respond to every probe with a pong addressed to the pinger.
            // The daemon owns this — no agent involvement. The pong is directed,
            // so `deliver` sends it unicast to the pinger; if the pinger's
            // `PeerInfo` hasn't arrived yet the pong is dropped (debug-logged)
            // and the pinger's round simply misses us this time.
            let pong = Message::new_pong(ctx.mesh, ctx.author, message.author.clone())
                .signed(ctx.identity);
            crate::logging::messages::log_out(&pong);
            if let Ok(bytes) = pong.serialize()
                && let Err(error) =
                    crate::transport::deliver(&pong, Bytes::from(bytes), state, ctx.sender).await
            {
                tracing::debug!(target: "agent_habilis_mesh::gossip", %error, pinger = %message.author, "auto-pong not delivered");
            }
            return;
        }
        MessageKind::Pong { to } => {
            // Record arrival for the active round only if addressed to us
            // and from a known peer — the roster gate bounds the
            // map and keeps `responded`/`known` honest against a peer that
            // forges pongs from fabricated authors.
            if to == ctx.author
                && state.peers.contains(message.author.as_str())
                && let Some(round) = state.ping_round.as_mut()
            {
                round
                    .pongs
                    .insert(message.author.clone(), tokio::time::Instant::now());
            }
            return;
        }
        MessageKind::State => {
            dispatch_channel(
                ChannelEvent {
                    channel: Channel::State,
                    message: &message,
                    surfaceable,
                },
                state,
                app,
                ctx,
            );
            return;
        }
        MessageKind::StateDigest => {
            antientropy::handle_state_digest(Channel::State, &message, state, ctx).await;
            return;
        }
        MessageKind::Meta => {
            dispatch_channel(
                ChannelEvent {
                    channel: Channel::Meta,
                    message: &message,
                    surfaceable,
                },
                state,
                app,
                ctx,
            );
            return;
        }
        MessageKind::MetaDigest => {
            antientropy::handle_state_digest(Channel::Meta, &message, state, ctx).await;
            return;
        }
        MessageKind::LinkState => {
            handle_link_state(&message, state);
            return;
        }
        MessageKind::Presence { subtype } => {
            lifecycle::handle_presence(
                lifecycle::PresenceEvent {
                    message: &message,
                    subtype: *subtype,
                    update: &observed.update,
                    surfaceable,
                },
                state,
                app,
                ctx,
            )
            .await;
        }
        MessageKind::App { .. } => {
            // The app-payload seam: dispatch RPC legs / chat / task frames.
            // `false` means the frame is plumbing or addressed elsewhere —
            // stop before retention.
            if !app
                .on_app_frame(
                    InboundApp {
                        message: &message,
                        surfaceable,
                    },
                    state,
                    ctx,
                )
                .await
            {
                return;
            }
        }
    }

    // Restore the sealed body (if we decrypted a directed frame for ourselves)
    // so the message log + anti-entropy re-serve the exact signed wire frame.
    if let Some(sealed) = sealed_body {
        message.body = sealed;
    }
    retain_and_index(
        message,
        RetainMeta {
            canonical: &canonical,
            class: app_class.as_ref(),
        },
        state,
        ctx,
    );
}

/// Fold a received multihop link-state advertisement into the routing table. The
/// author's own measured links + underlay address (`body` = a serialized
/// `LinkVector`) supersede the last vector we held for that origin. Dropped
/// silently when the multihop transport is off or the body is malformed —
/// plumbing like the digests, never logged or surfaced.
fn handle_link_state(message: &Message, state: &mut EventLoopState) {
    let Some(handle) = state.multihop.as_ref() else {
        return; // multihop off: nothing consumes the routing table
    };
    match serde_json::from_str::<iroh_multihop_transport::LinkVector>(message.body.as_str()) {
        Ok(vector) => {
            let updated = handle.feed_topology(vector);
            tracing::debug!(target: "agent_habilis_mesh::gossip",
                author = %message.author,
                updated,
                "multihop link-state received"
            );
        }
        Err(error) => {
            tracing::debug!(target: "agent_habilis_mesh::gossip",
                author = %message.author,
                %error,
                "dropping malformed multihop link-state"
            );
        }
    }
}

struct InboundFrame<'a> {
    message: &'a Message,
    canonical: &'a [u8],
    surfaceable: bool,
}

/// One shard of a split body: retain it for anti-entropy (so a missing shard
/// heals like any message) and, when its group completes, gate the
/// reassembled logical message through the application boundary and surface it once —
/// pushed and dispatched exactly like an ordinary inbound message.
async fn handle_shard(
    frame: InboundFrame<'_>,
    state: &mut EventLoopState,
    app: &mut dyn NodeApp,
    ctx: &HandlerCtx<'_>,
) {
    let InboundFrame {
        message,
        canonical,
        surfaceable,
    } = frame;
    // A directed shard (task leg / RPC) not addressed to us is relayed at the
    // gossip layer but never retained or reassembled here — a third party must
    // not persist or reconstruct a task exchange it is not part of. The
    // single-frame path drops non-addressee directed frames the same way.
    if !addressed_to_us(message, ctx.author) {
        return;
    }
    // The raw shard's own per-tag policy (loggability/chain) — computed from its
    // fragment body, matching the pre-thread behavior of the internal classify.
    let shard_class = message.kind.app_tag().map(|_| app.classify(message));
    // Only a small group's shards enter the message log (anti-entropy heals
    // them like any message); a big group would evict the mesh's whole
    // anti-entropy history, so its shards stay out of the log — on the send
    // side too (`shard_fits_log`) — and the group heals via shard repair.
    // (RPC shards are plumbing — `retain_and_index` skips them by kind
    // either way.)
    if crate::protocol::message::shard_fits_log(message) {
        retain_and_index(
            message.clone(),
            RetainMeta {
                canonical,
                class: shard_class.as_ref(),
            },
            state,
            ctx,
        );
    }
    if let Some(mut logical) = state.reassemble(message) {
        // Only the addressee reaches here (non-addressed directed shards returned
        // above), so decrypt the reassembled directed body before validating it —
        // but only when the app marks this tag sealed (its own convention). A
        // plaintext-directed consumer's body is already final.
        if app.classify(&logical).sealed && !decrypt_directed(&mut logical, state, ctx) {
            return;
        }
        // A broadcast frame reassembles to a body that, on a passworded mesh, is
        // mesh-key sealed; decrypt it (the retained shards above stay ciphertext)
        // before validating + surfacing, rejecting an unsealed/unopenable body.
        if matches!(&logical.kind, MessageKind::App { to: None, .. })
            && state.broadcast_key.is_some()
            && decrypt_broadcast(&mut logical, state).is_none()
        {
            return;
        }
        // Classify the reassembled body once; thread it to the push step.
        let logical_class = logical.kind.app_tag().map(|_| app.classify(&logical));
        if logical_class.as_ref().is_some_and(|cls| !cls.valid) {
            tracing::warn!(target: "agent_habilis_mesh::gossip",
                author = %logical.author,
                "dropping reassembled app frame with an invalid payload"
            );
            return;
        }
        maybe_push_inbound(ctx, &logical, surfaceable, logical_class.as_ref());
        // Dispatch the reassembled logical frame exactly like a single-frame one:
        // a directed RPC request/response (both shard now) reaches its handler,
        // and content surfaces. The shards were already retained above, so the
        // retain decision `on_app_frame` returns is ignored here.
        let _ = app
            .on_app_frame(
                InboundApp {
                    message: &logical,
                    surfaceable,
                },
                state,
                ctx,
            )
            .await;
    }
}

/// Fork detection, cross-author DAG folding, and chat-log retention for a
/// surviving inbound message. Coupled to logging on purpose: only a message we
/// actually **retain** is folded into the indexes, so `by_hash`/`dag_heads`/
/// `author_seqs` stay bounded by the log window (pruned on eviction). A
/// rate-dropped chat frame, one addressed to another peer, or a `State` event
/// returned earlier and never reaches here. Only a **broadcast chat** `app_msg`
/// carries `seq`/parents; task-related `app_msg` frames (offer/context/change)
/// are presence-like — `broadcast_task` stamps them without a chain, so they
/// must stay out of the fork/DAG indexes (see the `task` glossary invariant).
/// Presence is loggable but not indexed.
#[derive(Clone, Copy)]
struct RetainMeta<'a> {
    canonical: &'a [u8],
    class: Option<&'a AppClass>,
}

fn retain_and_index(
    message: Message,
    meta: RetainMeta<'_>,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    let RetainMeta { canonical, class } = meta;
    // App frames carry the app's per-tag policy (`classify`, computed once by the
    // caller and threaded in here); infra kinds (`class == None`) use the engine's
    // own rule. A beat is plumbing — never retained/indexed.
    let loggable = class.map_or_else(|| is_loggable(&message.kind), |cls| cls.loggable);
    let beat = class.is_some_and(|cls| cls.beat);
    if !loggable || beat {
        return;
    }
    if class.is_some_and(|cls| cls.chained) {
        let hash = identity::content_hash_hex(canonical);
        // A second, *different* content hash at an already-seen `(pubkey, seq)`
        // is cryptographic proof of equivocation — surface a `fork` once per
        // offending key (order-independent; we keep both).
        if let Some(seq) = message.seq
            && state.note_msg_seq(&message.pubkey, seq, hash.clone())
        {
            ctx.sink.emit(NodeEvent::Fork {
                nickname: message.author.clone(),
                pubkey: message.pubkey.clone(),
                seq,
            });
            tracing::warn!(target: "agent_habilis_mesh::gossip", author = %message.author, seq, "fork detected: conflicting messages at same seq");
        }
        // Fold into the DAG tip set; flag a backdated timestamp.
        if state.note_dag(hash, &message.parents, message.timestamp) {
            tracing::warn!(target: "agent_habilis_mesh::gossip", author = %message.author, "message timestamp precedes a referenced parent; possible backdating");
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

/// Route a received `State`/`Meta` event to its channel doc, then — for `meta` —
/// adopt the author's endpoint hint into the dial book. Selects the disjoint doc
/// field so the borrow released by [`ingest_channel_event`] frees `state` for the
/// address adoption.
#[derive(Clone, Copy)]
struct ChannelEvent<'a> {
    channel: Channel,
    message: &'a Message,
    surfaceable: bool,
}

fn dispatch_channel(
    event: ChannelEvent<'_>,
    state: &mut EventLoopState,
    app: &mut dyn NodeApp,
    ctx: &HandlerCtx<'_>,
) {
    let ChannelEvent {
        channel, message, ..
    } = event;
    let doc = match channel {
        Channel::State => &mut state.state_doc,
        Channel::Meta => &mut state.meta_doc,
    };
    ingest_channel_event(event, doc, ctx);
    if channel == Channel::Meta {
        // Post-apply hook: let the app adopt the author's endpoint hint from the
        // freshly-synced meta doc (the borrow above released `state`).
        app.on_meta_applied(&message.author, state, ctx);
    }
}

/// A plaintext-bodied view of `message` for surfacing: when `plain` differs from
/// the wire body (a decrypted `enc` body), a clone with the body replaced; else
/// the original borrowed unchanged. The output helpers read `event.body` for the
/// `m` delta, so they must see plaintext.
fn surface_view(message: &Message, plain: Option<String>) -> std::borrow::Cow<'_, Message> {
    match plain {
        Some(text) if text != message.body.as_str() => {
            let mut clone = message.clone();
            if let Ok(body) = MessageBody::new(text) {
                clone.body = body;
            }
            std::borrow::Cow::Owned(clone)
        }
        _ => std::borrow::Cow::Borrowed(message),
    }
}

/// Ingest a durable channel event (`State`/`Meta`) into its `log`: dedup-insert,
/// then surface a `StateChanged` for `channel` only when the derived doc changed
/// and we're past the join horizon (a backfilling joiner updates its doc silently
/// instead of reacting to replayed intermediate states). A received event is
/// never our own (`is_self = false`). Both channels share this path verbatim.
fn ingest_channel_event(
    event: ChannelEvent<'_>,
    doc: &mut crate::doc::MeshDoc,
    ctx: &HandlerCtx<'_>,
) {
    use crate::doc::Ingested;

    let ChannelEvent {
        channel,
        message,
        surfaceable,
    } = event;
    // The doc dedups by change hash, buffers out-of-order changes until their
    // deps land, retains the signed frame for re-serve, and gates card forgery
    // (the card carries the peer's cryptographic identity) — every recipient
    // applies the same gate, so a forgery converges nowhere.
    match doc.ingest(message) {
        Ingested::Applied {
            changed: true,
            doc: after,
        } if surfaceable => {
            // On a passworded mesh the received body is ciphertext; surface a
            // plaintext-bodied view so the human/JSON `m` delta still renders.
            // The frame retained for re-serve (inside `ingest`) stays ciphertext.
            let surfaced = surface_view(message, doc.surface_body(message));
            ctx.sink.emit(NodeEvent::StateChanged {
                channel,
                event: Box::new(surfaced.into_owned()),
                document: after,
                is_self: false,
            });
        }
        Ingested::Rejected => {
            tracing::warn!(target: "agent_habilis_mesh::gossip",
                author = %message.author,
                "dropping a {} change that forges another peer's agent card",
                channel.label()
            );
        }
        Ingested::Applied { .. } | Ingested::Duplicate | Ingested::Buffered | Ingested::Ignored => {
        }
    }
}

/// A directed leg (a directed `app_msg` or a `Task`) is surfaced **only by
/// its addressee** — a third party relays it without ever seeing it
/// (glossary: "a third party never sees it"). Broadcast (`to: None`) and
/// non-directed kinds are for everyone. Our own echoes never reach the
/// receive path (the pubkey self-echo gate drops them earlier), so on receive
/// "addressee" is the whole rule. This is the same gate the print/log path
/// applies in `lifecycle::{handle_msg, handle_task}`.
/// A structurally **directed** app frame — one with an addressee (`to:
/// Some(_)`). Whether its body is *sealed* (encrypted to that addressee) is a
/// separate, app-driven decision ([`super::app::AppClass::sealed`]): an app's
/// directed tags arrive sealed, a plaintext-directed consumer's do not. `Pong`
/// is directed for routing but a distinct infra kind, so it is not covered here.
fn is_directed_content(kind: &MessageKind) -> bool {
    matches!(kind, MessageKind::App { to: Some(_), .. })
}

/// If `message` is a directed payload sealed **to us**, decrypt its body in
/// place so the surfacing pipeline sees plaintext. Returns `false` only when a
/// frame sealed to us fails to open (tampered / wrong key), so the caller drops
/// it. A relay (not the addressee), broadcast, and plumbing pass through
/// untouched — the caller only invokes this on the addressee's path.
/// Outcome of the inbound gate. `Pass` carries the original **sealed** body to
/// restore before retention (`sealed`: `Some` when we decrypted a directed or
/// mesh-key body addressed to us, so anti-entropy re-serves the wire frame not
/// our decrypted view; `None` otherwise) plus the frame's [`AppClass`] (`class`:
/// `Some` for an App frame, classified once here on the decrypted body; `None`
/// for an infra frame with no app payload). Threading the class out means the
/// push + retain steps never re-parse the payload.
enum Gated {
    Drop,
    Pass {
        sealed: Option<MessageBody>,
        class: Option<AppClass>,
    },
}

/// Classify an App frame and enforce the app boundary gate in one parse:
/// returns the [`AppClass`] on a valid payload, `None` (logging the drop) when
/// the payload fails its frame's contract. Callers reuse the returned class
/// rather than re-classifying.
fn gate_app_payload(message: &Message, app: &dyn NodeApp) -> Option<AppClass> {
    let class = app.classify(message);
    if class.valid {
        Some(class)
    } else {
        tracing::warn!(target: "agent_habilis_mesh::gossip",
            author = %message.author,
            "dropping app frame with an invalid payload"
        );
        None
    }
}

/// Validate + (for the addressee) decrypt an inbound **non-sharded** logical
/// frame before surfacing, returning the once-computed [`AppClass`] alongside
/// the sealed-body-to-restore.
fn gate_and_decrypt(
    message: &mut Message,
    state: &EventLoopState,
    ctx: &HandlerCtx<'_>,
    app: &dyn NodeApp,
) -> Gated {
    if is_directed_content(&message.kind) {
        if !addressed_to_us(message, ctx.author) {
            // Relay: an opaque directed frame — forwarded + retained, never
            // validated or surfaced (the dispatch below is gated to the
            // addressee), so it needs no classification here.
            return Gated::Pass {
                sealed: None,
                class: None,
            };
        }
        return gate_directed_to_us(message, state, ctx, app);
    }
    // A broadcast app frame on a passworded mesh must arrive sealed with the
    // mesh key: decrypt in place so validation + surfacing see plaintext,
    // rejecting an unsealed or unopenable body (cleartext can't slip past the
    // guarantee). Only broadcast MSG is ever mesh-key sealed — a structural
    // `App{to:None}` gate is behavior-identical to the former `is_app(MSG)` one.
    // The ciphertext is returned so `ingest` restores it before retention.
    if matches!(&message.kind, MessageKind::App { to: None, .. }) && state.broadcast_key.is_some() {
        let Some(sealed) = decrypt_broadcast(message, state) else {
            return Gated::Drop;
        };
        let Some(class) = gate_app_payload(message, app) else {
            return Gated::Drop;
        };
        return Gated::Pass {
            sealed: Some(sealed),
            class: Some(class),
        };
    }
    // Broadcast app / infra logical frames validate in place. An infra kind has
    // no app payload — it passes with `class: None`.
    match message.kind.app_tag() {
        Some(_) => match gate_app_payload(message, app) {
            Some(class) => Gated::Pass {
                sealed: None,
                class: Some(class),
            },
            None => Gated::Drop,
        },
        None => Gated::Pass {
            sealed: None,
            class: None,
        },
    }
}

/// Validate + decrypt a directed frame that's addressed to us (as opposed to
/// one we're only relaying).
fn gate_directed_to_us(
    message: &mut Message,
    state: &EventLoopState,
    ctx: &HandlerCtx<'_>,
    app: &dyn NodeApp,
) -> Gated {
    // Whether the directed body is sealed is the app's call, not a
    // structural given: an app's directed tags may arrive encrypted, but a
    // consumer with its own data model may send plaintext directed bytes. The frame is
    // signature-authenticated either way; `sealed=false` only means "not
    // additionally encrypted".
    if !app.classify(message).sealed {
        // Plaintext directed body: the wire body is already the final body,
        // so retain_and_index re-serves it unchanged (`sealed: None`).
        let Some(class) = gate_app_payload(message, app) else {
            return Gated::Drop;
        };
        return Gated::Pass {
            sealed: None,
            class: Some(class),
        };
    }
    let sealed = message.body.clone();
    if !decrypt_directed(message, state, ctx) {
        return Gated::Drop;
    }
    let Some(class) = gate_app_payload(message, app) else {
        return Gated::Drop;
    };
    Gated::Pass {
        sealed: Some(sealed),
        class: Some(class),
    }
}

/// Decrypt a broadcast chat body sealed under the mesh key, **in place**.
/// Returns the original (ciphertext) body to restore before retention, or `None`
/// when there is no key or the body is not a decryptable `enc` envelope (on a
/// passworded mesh that means an unsealed/tampered body — the caller drops it).
fn decrypt_broadcast(message: &mut Message, state: &EventLoopState) -> Option<MessageBody> {
    let key = state.broadcast_key.as_deref()?;
    // A key exists (passworded mesh), so a body that will not open is either
    // cleartext or tampered — drop it, but say so (mirroring the directed-drop
    // warn) rather than vanishing silently. A generic `send_app` broadcast that
    // did not mesh-key-seal its body lands here; see `send_app`'s doc.
    let Some(plain) = crate::doc::wire::decrypt_body(message.body.as_str(), Some(key)) else {
        tracing::warn!(target: "agent_habilis_mesh::gossip",
            author = %message.author,
            "dropping a broadcast body that failed mesh-key decryption (cleartext on a passworded mesh?)"
        );
        return None;
    };
    let body = MessageBody::new(plain).ok()?;
    Some(std::mem::replace(&mut message.body, body))
}

fn decrypt_directed(message: &mut Message, state: &EventLoopState, ctx: &HandlerCtx<'_>) -> bool {
    if !is_directed_content(&message.kind) || !addressed_to_us(message, ctx.author) {
        return true;
    }
    match crate::protocol::seal::unseal_from_body(&state.identity.seal_secret(), &message.body) {
        Ok(plaintext) => match MessageBody::new(plaintext) {
            Ok(body) => {
                message.body = body;
                true
            }
            Err(_) => false,
        },
        Err(error) => {
            tracing::warn!(target: "agent_habilis_mesh::gossip", %error, author = %message.author, "dropping a directed frame sealed to us that failed to open");
            false
        }
    }
}

fn addressed_to_us(message: &Message, us: &Nickname) -> bool {
    match &message.kind {
        MessageKind::App { to: Some(to), .. } => to == us,
        // Broadcast app frames (chat) and infra kinds are delivered to everyone.
        MessageKind::App { to: None, .. }
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::State
        | MessageKind::Meta
        | MessageKind::LinkState => true,
    }
}

/// Hand a surviving inbound message to the in-process driver before kind
/// routing, so the consumer sees `msg` / `presence` / `peer_info` /
/// `task` alike. Non-blocking by construction (bounded broadcast); a send error
/// or full ring is intentionally dropped so a slow consumer never stalls
/// the gossip loop. The `receiver_count` gate skips the per-message clone while
/// nobody has subscribed yet; a consumer that never wants the fan-out passes no
/// sender at all, so the field is `None` and even that gate is skipped.
/// `surfaceable` keeps pre-join backlog off the inbound push channel too; plumbing
/// kinds (digest/ping/pong) are excluded. `class` is the caller's once-computed
/// classification (`None` for an infra frame).
fn maybe_push_inbound(
    ctx: &HandlerCtx<'_>,
    message: &Message,
    surfaceable: bool,
    class: Option<&AppClass>,
) {
    // Short-circuit the cheap structural guards first: no live inbound subscriber
    // (always so for CLI/MCP, where the field is `None`), a pre-join or
    // third-party frame, or an infra kind.
    let Some(tx) = ctx.external_msg_tx else {
        return;
    };
    if tx.receiver_count() == 0
        || !surfaceable
        || !addressed_to_us(message, ctx.author)
        || matches!(
            message.kind,
            MessageKind::Digest
                | MessageKind::StateDigest
                | MessageKind::MetaDigest
                | MessageKind::State
                | MessageKind::Meta
                | MessageKind::Ping
                | MessageKind::Pong { .. }
        )
    {
        return;
    }
    // An App frame reaches an in-process consumer only when it is loggable (not RPC
    // plumbing) and not a liveness beat — the app's per-tag policy, threaded in
    // from the caller's single classification. An infra frame (`class == None`,
    // `app_tag` also `None`) is always pushable past the structural guards above.
    let app_pushable = match message.kind.app_tag() {
        Some(_) => class.is_some_and(|cls| cls.loggable && !cls.beat),
        None => true,
    };
    if app_pushable {
        let _ = tx.send(message.clone());
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
    if peer_id == ctx.endpoint.id() {
        return;
    }
    // This `PeerInfo` is signed by `message.author` and carries that author's
    // own endpoint — the one binding from a nickname to a node id. Record it
    // for the roster's `direct`/`gossip` tag; last-writer-wins, so a restart's
    // fresh endpoint replaces the old. The beacon gossips *as* the rendezvous,
    // so it advertises `rendezvous_id` here — record that too, so a joiner can
    // tell its rendezvous link belongs to the beacon's nickname.
    state.peer_endpoints.insert(message.author.clone(), peer_id);
    // The rendezvous is overlay plumbing, not a dialable member peer:
    // the binding above is recorded, but skip the known-endpoints/re-bridge
    // dial path for it.
    if peer_id == ctx.rendezvous_id {
        return;
    }
    // Remember every advertised peer for the rendezvous-independent
    // re-bridge, and register its address once. Unconditional on link
    // state — a `PeerInfo` normally arrives over an already-formed link,
    // and recovery (`heal::rebridge_known`, the starvation watchdog's
    // precondition) needs the memory precisely *after* that link dies.
    let first_sighting = state.known_endpoints.insert(peer_id);
    if first_sighting {
        let _ = add_peer_addr(ctx.endpoint, peer_addr.clone());
    }
    // A buffered directed frame addressed to this author may just have become
    // deliverable — its endpoint binding and dial address are now registered.
    // Only while meshed: the pre-mesh backlog waits for the meshed edge.
    if state.meshed && !state.pending_outbound.is_empty() {
        flush_pending(state, ctx, "peer info arrival").await;
    }
    let now = Instant::now();
    // Simultaneous-open tie-break: on a peer's *first* sighting, only the
    // numerically lower endpoint id dials.
    //
    // A join floods its `PeerInfo` to everyone at once, so both sides of a new
    // pair reach this dial in the same millisecond and each opens a connection.
    // iroh-gossip does dedup the pair — but each side independently closes the
    // connection *it* dialed, and those are the two different halves, so both
    // die and the pair is left unlinked. Observed directly: `neighbor up` for
    // both, then `remaining 1 other connections`, then `closed by peer: close
    // from disconnect` ~23ms later, leaving each node in `silent partition
    // (meshed but zero peer links)` — which is how a graceful `Left` came to be
    // broadcast into the void, since by then the leaver had no neighbors.
    //
    // Comparing ids gives both sides the same answer without a round trip.
    // Deliberately scoped to the first sighting, so it defers a dial rather
    // than vetoing one: if the lower id never dials (it may not hold our
    // `PeerInfo` yet), the next re-receipt past the relink cooldown dials from
    // this side after all. The rendezvous is exempt — it returned above — so
    // the bootstrap link is never subject to this.
    let defer_first_dial = first_sighting && ctx.endpoint.id() > peer_id;
    if defer_first_dial {
        tracing::debug!(target: "agent_habilis_mesh::gossip",
            endpoint_id = %peer_id,
            "deferring first dial to the lower endpoint id (simultaneous-open tie-break)"
        );
    }
    // A `PeerInfo` is a dial hint, never a link: `linked_endpoints` is
    // owned by `NeighborUp`/`NeighborDown` (link truth), so an unlinked
    // peer's `PeerInfo` only re-registers its (possibly fresh) address
    // and *asks* the gossip actor to graft it. Until the link
    // materializes, each post-cooldown re-receipt retries the dial —
    // the healer's per-peer backstop.
    if !defer_first_dial
        && state.linked_endpoints.len() < ctx.max_peers
        && !state.linked_endpoints.contains(&peer_id)
        && !state.relink_on_cooldown(peer_id, now)
    {
        state.note_relink(peer_id, now);
        let _ = add_peer_addr(ctx.endpoint, peer_addr);
        if let Err(error) = ctx.sender.join_peers(vec![peer_id]).await {
            tracing::warn!(target: "agent_habilis_mesh::gossip", endpoint_id = %peer_id, %error, "PeerInfo graft request failed");
        }
        let _ = ctx.sender.broadcast(content).await;
        state.last_sent_at = Instant::now();
        tracing::debug!(target: "agent_habilis_mesh::gossip",
            endpoint_id = %peer_id,
            linked = state.linked_endpoints.len(),
            "dialing peer from PeerInfo"
        );
    }
}

/// `Alive` keepalives, `PeerInfo` endpoint plumbing, anti-entropy `Digest`s,
/// and ping/pong probes are infrastructure; everything else (real chat frames
/// and `joined`/`left` presence) goes in the log (and so to `poll`/`fetch`).
/// App-frame loggability is the app's own per-tag policy ([`NodeApp::classify`]);
/// this predicate covers only the engine's infra kinds.
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
            | MessageKind::Digest | MessageKind::StateDigest | MessageKind::MetaDigest
            | MessageKind::Ping
            | MessageKind::Pong { .. }
            // Durable state lives in its own un-pruned log, never the chat
            // message-log / poll-fetch buffer (it also returns before reaching
            // here; this keeps the predicate honest if that changes).
            | MessageKind::State
            | MessageKind::Meta
            // App-frame loggability is decided by the app's `classify`, never
            // this infra predicate; an App kind never reaches here.
            | MessageKind::App { .. }
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
    fn peerinfo_is_not_loggable() {
        // PeerInfo is endpoint plumbing — never logged/surfaced, and the
        // classifier (not just its early-return) says so.
        assert!(!is_loggable(&MessageKind::PeerInfo));
    }
}
