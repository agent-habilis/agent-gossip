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
use crate::protocol::message::MessageBody;
use crate::protocol::{Channel, Message, MessageKind, Nickname};
use crate::util::tuning::RECLAIM_WINDOW_SECS;

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
    ctx: &HandlerCtx<'_>,
) {
    match event {
        Some(Ok(Event::Received(received))) => {
            ingest(received.content, state, ctx).await;
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
                    // Re-publish our card now that we're meshed: the home relay
                    // is homed by this point, so the card's endpoint hint gets a
                    // real dial path (the startup publish may have had none). A
                    // no-op if the address is unchanged.
                    crate::daemon::event_loop::publish_own_card(ctx, state).await;
                }
            }
        }
        Some(Ok(Event::NeighborDown(node_id))) => {
            let is_rendezvous = node_id == ctx.rendezvous_id;
            tracing::info!(endpoint_id = %node_id, is_rendezvous, "gossip neighbor down");
            if is_rendezvous {
                state.rendezvous_linked = false;
            } else {
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
                ingest(incoming.content, state, ctx).await;
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
/// Process an inbound task leg (a task-related `a2a_msg`, an `a2a_status`,
/// or an `a2a_artifact`) for its addressee: advance the coarse state machine
/// (addressee only — a third-party relay tracks nothing), then surface it via
/// the lifecycle layer. Returns `false` when the caller should stop
/// processing this message (it is not loggable past this point).
fn handle_task_leg(
    message: &Message,
    to: &Nickname,
    surfaceable: bool,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) -> bool {
    // Only the addressee is a party: a third-party relay tracks nothing.
    if to == ctx.author {
        crate::a2a::task::ingest(&mut state.tasks, message, false, Instant::now());
    }
    lifecycle::handle_task(ctx.output, message, to, surfaceable, ctx.author)
}

/// Surface a reassembled multipart body (a `Msg` or a `Task` content leg)
/// through the same lifecycle path an unsplit message of that kind takes. The
/// raw shards were already retained; this only surfaces the logical view, so it
/// does **not** re-retain or re-index.
fn surface_logical(
    logical: &Message,
    surfaceable: bool,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    match &logical.kind {
        MessageKind::A2aMsg => {
            // `A2aMsg` is broadcast chat (task legs never shard through here —
            // `message/send` rides RPC, status/artifact are small).
            lifecycle::handle_msg(ctx.output, logical, surfaceable);
        }
        MessageKind::A2aStatus { to, .. } | MessageKind::A2aArtifact { to, .. } => {
            handle_task_leg(logical, to, surfaceable, state, ctx);
        }
        // Only content frames are ever split (the sender refuses to split
        // anything else), so other kinds never reach here.
        MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::A2aReq { .. }
        | MessageKind::A2aResp { .. }
        | MessageKind::State
        | MessageKind::Meta
        | MessageKind::LinkState => {}
    }
}

/// Validate + dispatch one inbound wire message, regardless of transport. Both
/// the gossip event pump (`handle_gossip_event`) and the unicast inbound arm
/// (`daemon::event_loop`) funnel here, so signature-verify, the swarm gate, and
/// `mark_seen` dedup are identical on both planes — a message delivered over
/// both surfaces exactly once. `message` is mutable so a directed frame sealed
/// to us can be decrypted in place before surfacing (see `gate_and_decrypt`).
pub(crate) async fn ingest(content: Bytes, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    let Ok(mut message) = Message::parse(&content) else {
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
    // Swarm gate: the gossip topic already isolates honest swarms, but a peer
    // crafted onto our topic could stamp an arbitrary (signed) `swarm` string
    // that would otherwise flow unchecked into the `--output json` agent API.
    // Drop anything not stamped with our own swarm id.
    if message.swarm != *ctx.swarm {
        tracing::warn!(author = %message.author, "dropping message stamped with a foreign swarm id");
        return;
    }
    // Starvation watchdog signal, *before* dedup: even a duplicate
    // delivery proves the mesh carries traffic. On the degraded→meshed
    // edge (recovery succeeded) the outbound buffer flushes here.
    if state.note_inbound(Instant::now()) {
        flush_pending(state, ctx, "inbound traffic resumed").await;
    }
    // Duplicate suppression: a true repeat delivery must not
    // re-heartbeat, re-run membership, re-embed-forward, re-log, or re-print.
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
    // so a nickname is never "burned" by a restart on a long-lived swarm.
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
        handle_shard(&message, &canonical, surfaceable, state, ctx).await;
        return;
    }

    // Directed frames are sealed to their addressee. A **relay** forwards and
    // retains the opaque frame but never validates or surfaces its content (it
    // can't read it, and `maybe_push_embed`/dispatch below are already gated to
    // the addressee). The **addressee** decrypts the body in place, then the
    // recovered plaintext passes the A2A boundary gate exactly as a broadcast
    // does. A frame sealed to us that fails to open (tampered / wrong key) is
    // dropped.
    let sealed_body = match gate_and_decrypt(&mut message, state, ctx) {
        Gated::Pass(sealed) => sealed,
        Gated::Drop => return,
    };

    maybe_push_embed(ctx, &message, surfaceable);

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
            // so `deliver` sends it unicast to the pinger when reachable that
            // way, avoiding the N-flooded-pongs fan-out; else it rides gossip.
            let pong = Message::new_pong(ctx.swarm, ctx.author, message.author.clone())
                .signed(ctx.identity);
            crate::logging::messages::log_out(&pong);
            if let Ok(bytes) = pong.serialize() {
                let _ = crate::unicast::deliver(&pong, Bytes::from(bytes), state, ctx.sender).await;
            }
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
        MessageKind::State => {
            dispatch_channel(Channel::State, &message, surfaceable, state, ctx);
            return;
        }
        MessageKind::StateDigest => {
            antientropy::handle_state_digest(Channel::State, &message, state, ctx).await;
            return;
        }
        MessageKind::Meta => {
            dispatch_channel(Channel::Meta, &message, surfaceable, state, ctx);
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
                &message,
                *subtype,
                &observed.update,
                surfaceable,
                state,
                ctx,
            )
            .await;
        }
        MessageKind::A2aReq { .. } | MessageKind::A2aResp { .. } => {
            handle_a2a_rpc(&message, state, ctx).await;
            return;
        }
        MessageKind::A2aMsg | MessageKind::A2aStatus { .. } | MessageKind::A2aArtifact { .. } => {
            if !route_content(&message, surfaceable, state, ctx) {
                return;
            }
        }
    }

    // Restore the sealed body (if we decrypted a directed frame for ourselves)
    // so the message log + anti-entropy re-serve the exact signed wire frame.
    if let Some(sealed) = sealed_body {
        message.body = sealed;
    }
    retain_and_index(message, &canonical, state, ctx);
}

/// Fold a received relay link-state advertisement into the routing table. The
/// author's own measured links (`body` = a serialized `LinkVector`) supersede
/// the last vector we held for that origin. Plumbing like the digests — never
/// logged or surfaced; a malformed body is dropped.
fn handle_link_state(message: &Message, state: &mut EventLoopState) {
    match crate::circuit::LinkVector::from_json(message.body.as_str()) {
        Ok(vector) => {
            let updated = state.link_state.ingest(vector);
            tracing::debug!(
                target: crate::circuit::LOG_TARGET,
                author = %message.author,
                updated,
                "relay link-state received"
            );
        }
        Err(error) => {
            tracing::debug!(
                target: crate::circuit::LOG_TARGET,
                author = %message.author,
                %error,
                "dropping malformed relay link-state"
            );
        }
    }
}

/// Route an A2A RPC frame: a request addressed to us is served (and a
/// response broadcast back); a response to one of our calls resolves its
/// waiter (matched by `rpc_id` *and* the responding peer). Frames addressed
/// to another peer are relayed only, never handled here. Plumbing — never
/// logged/retained.
async fn handle_a2a_rpc(message: &Message, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    match &message.kind {
        MessageKind::A2aReq { to, rpc_id } if to == ctx.author => {
            handle_a2a_req(message, &rpc_id.clone(), state, ctx).await;
        }
        MessageKind::A2aResp { to, rpc_id } if to == ctx.author => {
            // Only act on a response to a call WE actually issued (a matching
            // outstanding waiter). An unsolicited A2aResp is ignored — otherwise
            // any member could inject phantom initiator-side task records into
            // the uncapped task table by forging responses.
            if state.has_a2a_waiter(rpc_id, &message.author) {
                // Adopt the authoritative `Task` a `SendMessage` returned, so our
                // (initiator) daemon holds the record for lifecycle + surfacing
                // the worker's later pushes.
                state.adopt_returned_task(&message.author, message.body.as_str());
                state.fulfill_a2a_waiter(rpc_id, &message.author, message.body.as_str());
            }
        }
        // Not for us (relay only), or a non-RPC kind the caller mis-routed.
        MessageKind::A2aReq { .. }
        | MessageKind::A2aResp { .. }
        | MessageKind::A2aMsg
        | MessageKind::A2aStatus { .. }
        | MessageKind::A2aArtifact { .. }
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::State
        | MessageKind::Meta
        | MessageKind::LinkState => {}
    }
}

/// Serve one inbound `A2aReq` addressed to us: classify the JSON-RPC request
/// against the safe method set (`a2a::gossip_rpc::classify`), run the
/// resulting action, and broadcast an `A2aResp` back to the requester echoing
/// `rpc_id`. Runs inline in the event loop (`&mut state`), so it calls
/// `handle_op` directly — no channel round-trip.
async fn handle_a2a_req(
    message: &Message,
    rpc_id: &crate::a2a::A2aRpcId,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    use crate::a2a::gossip_rpc::Served;
    let requester = message.author.clone();
    // Parse the JSON-RPC envelope; a malformed request is a parse-error reply.
    let outcome: Result<serde_json::Value, crate::a2a::rpc::RpcError> = match serde_json::from_str::<
        serde_json::Value,
    >(
        message.body.as_str(),
    ) {
        Ok(envelope) => {
            let method = envelope["method"].as_str().unwrap_or_default();
            match crate::a2a::gossip_rpc::classify(method, &envelope["params"], &requester, state) {
                Served::Op(op) => {
                    // No local HTTP port on the gossip path; only card ops read
                    // it, and those aren't in the safe set served here.
                    crate::a2a::rpc::handle_op(op, ctx, 0, state).await
                }
                Served::Ingest(payload) => ingest_remote_message(&payload, &requester, state, ctx),
                Served::ShardRepair { group, missing } => {
                    resend_cached_shards(&group, &missing, &requester, state, ctx).await
                }
                Served::Reject(error) => Err(error),
            }
        }
        Err(error) => Err(crate::a2a::rpc::RpcError::invalid_params(format!(
            "malformed JSON-RPC request: {error}"
        ))),
    };
    let response = match outcome {
        Ok(result) => serde_json::json!({ "result": result }),
        Err(error) => {
            serde_json::json!({ "error": { "code": error.code, "message": error.message } })
        }
    };
    let Ok(body) = MessageBody::new(response.to_string()) else {
        tracing::warn!("a2a rpc response body invalid");
        return;
    };
    // Seal the response to the requester — a relay forwards it but cannot read it.
    let body = match crate::gossip::broadcast::seal_directed(state, &requester, &body) {
        Ok(sealed) => sealed,
        Err(error) => {
            tracing::warn!(%error, "cannot seal a2a rpc response — dropping");
            return;
        }
    };
    // Directed reply over the shared RPC sender: unicast to the requester when
    // dialable (circuit/gossip otherwise), splitting a large result body
    // transparently — a `tasks/list` over many tasks no longer silently
    // exceeds the frame cap.
    if let Err(error) = crate::gossip::broadcast::send_directed_rpc(
        ctx.swarm,
        ctx.author,
        MessageKind::A2aResp {
            to: requester,
            rpc_id: rpc_id.clone(),
        },
        body,
        state,
        ctx.sender,
    )
    .await
    {
        tracing::warn!(%error, "a2a rpc response send failed");
    }
}

/// Serve a `shard/repair` ask: re-deliver the requested frames of one of our
/// cached big outbound groups. The frames are our own signed wire bytes, so a
/// re-delivery is indistinguishable from the original send; the requester's
/// dedup drops anything it already holds. Directed shards are re-sent only to
/// their addressee — anyone else gets nothing (the sealed body would be
/// unreadable to them anyway, but there is no reason to serve a third party).
async fn resend_cached_shards(
    group: &crate::protocol::ShardGroup,
    missing: &[u32],
    requester: &Nickname,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) -> Result<serde_json::Value, crate::a2a::rpc::RpcError> {
    let frames = state.shard_cache.frames(group, missing);
    let mut resent = 0usize;
    for bytes in frames {
        let Ok(msg) = Message::parse(&bytes) else {
            continue; // never: the cache holds frames we serialized ourselves
        };
        if let Some(to) = crate::protocol::message::sole_addressee(&msg.kind)
            && to != requester
        {
            continue;
        }
        if crate::unicast::deliver(&msg, bytes, state, ctx.sender).await.is_ok() {
            resent += 1;
        }
    }
    tracing::debug!(
        target: "agent_gossip::gossip",
        %group,
        requested = missing.len(),
        resent,
        "served a shard repair request"
    );
    Ok(serde_json::json!({ "resent": resent }))
}

/// A `message/send` directed at us over gossip-RPC — we are the task's A2A
/// **server** (the worker). A message with no `taskId` opens a new task (we
/// mint the id); one with a `taskId` is a follow-up (the initiator's answer /
/// approval / change request). Either way we advance our coarse machine,
/// surface the message to our skill, and return the authoritative `Task`.
fn ingest_remote_message(
    payload: &crate::a2a::Message,
    requester: &Nickname,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) -> Result<serde_json::Value, crate::a2a::rpc::RpcError> {
    use crate::a2a::task::{LegInfo, LegKind};
    let now = Instant::now();
    let (task_id, kind) = match &payload.task_id {
        Some(task_id) => (task_id.clone(), LegKind::Text),
        None => (crate::a2a::TaskId::random(), LegKind::Offer),
    };
    // A follow-up (taskId set) may only be driven by the task's own party. The
    // requester is signature-verified, but signature ≠ party: without this
    // check any swarm member could drive and read a task they are not part of.
    if kind == LegKind::Text {
        match state.tasks.get(&task_id) {
            None => {
                return Err(crate::a2a::rpc::RpcError::invalid_params(
                    "SendMessage names a taskId with no task on this peer",
                ));
            }
            Some(rec) if &rec.peer != requester => {
                return Err(crate::a2a::rpc::RpcError {
                    code: -32003,
                    message: "not a party to the task named by taskId".to_string(),
                });
            }
            Some(_) => {}
        }
    }
    crate::a2a::task::apply(
        &mut state.tasks,
        &LegInfo {
            task_id: &task_id,
            peer: requester,
            kind,
            mine: false,
            fraction: None,
        },
        now,
    );
    // Surface the incoming message to our skill (a `task` event, kind:message).
    let rec_state = state.tasks.get(&task_id).map(|rec| rec.state);
    let text = crate::a2a::gossip::display_text(payload);
    ctx.output.task_message(&crate::output::TaskMessageLeg {
        id: payload.message_id.as_str(),
        swarm: ctx.swarm.as_str(),
        author: requester.as_str(),
        peer: ctx.author.as_str(),
        task_id: task_id.as_str(),
        state: rec_state,
        text: &text,
        is_self: false,
    });
    // A2A v1.0 `SendMessage` returns a `SendMessageResponse` oneof — creation
    // yields `{"task": <Task>}`.
    state
        .tasks
        .get(&task_id)
        .map(|rec| serde_json::json!({ "task": crate::a2a::rpc::task_object(&task_id, rec, ctx.swarm) }))
        .ok_or_else(|| crate::a2a::rpc::RpcError::internal("task record missing after ingest"))
}

/// Route a content frame — chat, or one of the three task shapes — to its
/// lifecycle handler. Returns whether the frame is loggable (a directed
/// frame addressed elsewhere, or a liveness beat, is not).
fn route_content(
    message: &Message,
    surfaceable: bool,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) -> bool {
    match &message.kind {
        MessageKind::A2aMsg => lifecycle::handle_msg(ctx.output, message, surfaceable),
        MessageKind::A2aStatus { to, .. } | MessageKind::A2aArtifact { to, .. } => {
            handle_task_leg(message, to, surfaceable, state, ctx)
        }
        MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::A2aReq { .. }
        | MessageKind::A2aResp { .. }
        | MessageKind::State
        | MessageKind::Meta
        | MessageKind::LinkState => false,
    }
}

/// One shard of a split body: retain it for anti-entropy (so a missing shard
/// heals like any message) and, when its group completes, gate the
/// reassembled logical message through the A2A boundary and surface it once —
/// embed-pushed and dispatched exactly like an ordinary inbound message.
async fn handle_shard(
    message: &Message,
    canonical: &[u8],
    surfaceable: bool,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    // A directed shard (task leg / RPC) not addressed to us is relayed at the
    // gossip layer but never retained or reassembled here — a third party must
    // not persist or reconstruct a task exchange it is not part of. The
    // single-frame path drops non-addressee directed frames the same way.
    if !addressed_to_us(message, ctx.author) {
        return;
    }
    // Only a small group's shards enter the message log (anti-entropy heals
    // them like any message); a big group would evict the swarm's whole
    // anti-entropy history, so its shards stay out of the log — on the send
    // side too (`shard_fits_log`) — and the group heals via shard repair.
    // (RPC shards are plumbing — `retain_and_index` skips them by kind
    // either way.)
    if crate::protocol::message::shard_fits_log(message) {
        retain_and_index(message.clone(), canonical, state, ctx);
    }
    if let Some(mut logical) = state.reassemble(message) {
        // Only the addressee reaches here (non-addressed directed shards returned
        // above), so decrypt the reassembled directed body before validating it.
        if !decrypt_directed(&mut logical, state, ctx) {
            return;
        }
        // A reassembled RPC frame dispatches to the server/waiter path — RPC
        // is plumbing, never surfaced (the single-frame path routes it the
        // same way, just before the surfacing pipeline).
        if matches!(
            logical.kind,
            MessageKind::A2aReq { .. } | MessageKind::A2aResp { .. }
        ) {
            handle_a2a_rpc(&logical, state, ctx).await;
            return;
        }
        // A broadcast chat reassembles to a sealed body on a passworded swarm;
        // decrypt it (the retained shards above stay ciphertext) before
        // validating + surfacing, rejecting an unsealed/unopenable reassembly.
        if matches!(logical.kind, MessageKind::A2aMsg)
            && state.broadcast_key.is_some()
            && decrypt_broadcast(&mut logical, state).is_none()
        {
            return;
        }
        if !chat_payload_valid(&logical) {
            return;
        }
        maybe_push_embed(ctx, &logical, surfaceable);
        surface_logical(&logical, surfaceable, state, ctx);
    }
}

/// Fork detection, cross-author DAG folding, and chat-log retention for a
/// surviving inbound message. Coupled to logging on purpose: only a message we
/// actually **retain** is folded into the indexes, so `by_hash`/`dag_heads`/
/// `author_seqs` stay bounded by the log window (pruned on eviction). A
/// rate-dropped chat frame, one addressed to another peer, or a `State` event
/// returned earlier and never reaches here. Only a **broadcast chat** `a2a_msg`
/// carries `seq`/parents; task-related `a2a_msg` frames (offer/context/change)
/// are presence-like — `broadcast_task` stamps them without a chain, so they
/// must stay out of the fork/DAG indexes (see the `task` glossary invariant).
/// Presence is loggable but not indexed.
fn retain_and_index(
    message: Message,
    canonical: &[u8],
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    if !is_loggable(&message.kind) || is_beat_frame(&message) {
        return;
    }
    if matches!(message.kind, MessageKind::A2aMsg) {
        let hash = identity::content_hash_hex(canonical);
        // A second, *different* content hash at an already-seen `(pubkey, seq)`
        // is cryptographic proof of equivocation — surface a `fork` once per
        // offending key (order-independent; we keep both).
        if let Some(seq) = message.seq
            && state.note_msg_seq(&message.pubkey, seq, hash.clone())
        {
            ctx.output.fork(&message.author, &message.pubkey, seq);
            tracing::warn!(author = %message.author, seq, "fork detected: conflicting messages at same seq");
        }
        // Fold into the DAG tip set; flag a backdated timestamp.
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

/// Hand a surviving inbound message to the embed facade before kind
/// routing, so the consumer sees `msg` / `presence` / `peer_info` /
/// `task` alike. Non-blocking by construction (bounded broadcast); a send error
/// or full ring is intentionally dropped so a slow embedder never stalls
/// the gossip loop. The `receiver_count` gate skips the per-message clone
/// while no consumer is subscribed (always, until the embedder calls
/// `messages()`; forever for CLI/MCP where the field is `None`).
/// `surfaceable` keeps pre-join backlog off the embed channel too; plumbing
/// kinds (digest/ping/pong) are excluded.
/// Ingest a durable channel event (`State`/`Meta`) into its `log`: dedup-insert,
/// then surface a `StateChanged` for `channel` only when the derived doc changed
/// and we're past the join horizon (a backfilling joiner updates its doc silently
/// instead of reacting to replayed intermediate states). A received event is
/// never our own (`is_self = false`). Both channels share this path verbatim.
/// Route a received `State`/`Meta` event to its channel doc, then — for `meta` —
/// adopt the author's endpoint hint into the dial book. Selects the disjoint doc
/// field so the borrow released by [`ingest_channel_event`] frees `state` for the
/// address adoption.
fn dispatch_channel(
    channel: Channel,
    message: &Message,
    surfaceable: bool,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    let doc = match channel {
        Channel::State => &mut state.state_doc,
        Channel::Meta => &mut state.meta_doc,
    };
    ingest_channel_event(channel, doc, message, surfaceable, ctx);
    if channel == Channel::Meta {
        adopt_meta_endpoint(&message.author, state, ctx);
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

fn ingest_channel_event(
    channel: Channel,
    doc: &mut crate::daemon::doc::SwarmDoc,
    message: &Message,
    surfaceable: bool,
    ctx: &HandlerCtx<'_>,
) {
    // The doc dedups by change hash, buffers out-of-order changes until their
    // deps land, retains the signed frame for re-serve, and gates card forgery
    // (the card carries the peer's cryptographic identity) — every recipient
    // applies the same gate, so a forgery converges nowhere.
    use crate::daemon::doc::Ingested;
    match doc.ingest(message) {
        Ingested::Applied {
            changed: true,
            doc: after,
        } if surfaceable => {
            // On a passworded swarm the received body is ciphertext; surface a
            // plaintext-bodied view so the human/JSON `m` delta still renders.
            // The frame retained for re-serve (inside `ingest`) stays ciphertext.
            let surfaced = surface_view(message, doc.surface_body(message));
            ctx.output
                .state_changed(channel, surfaced.as_ref(), &after, false);
        }
        Ingested::Rejected => {
            tracing::warn!(
                author = %message.author,
                "dropping a {} change that forges another peer's agent card",
                channel.label()
            );
        }
        Ingested::Applied { .. } | Ingested::Duplicate | Ingested::Buffered | Ingested::Ignored => {
        }
    }
}

/// Adopt `author`'s published endpoint hint (`/peers/<nick>/card/endpoint`) into
/// the dial book — the meta-doc counterpart to [`handle_peer_info`]. It binds
/// `participant_endpoints[author] = eid` and seeds iroh's address book via
/// [`add_peer_addr`], so a directed unicast can reach a peer we learned about
/// from the synced meta doc even before its gossiped `PeerInfo` arrives (a
/// forgery-gated, authenticated supplement). Both are last-writer-wins /
/// additive, so this never conflicts with `PeerInfo`. Unlike `handle_peer_info`
/// it does **not** graft a gossip link (`join_peers`) — mesh formation stays
/// `PeerInfo`'s job; this only enables directed dialing.
fn adopt_meta_endpoint(author: &Nickname, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    let doc = state.meta_doc.to_json();
    let Some((peer_id, peer_addr)) = crate::a2a::card::peer_endpoint(&doc, author) else {
        return;
    };
    // Never bind our own endpoint or the rendezvous pseudo-node.
    if peer_id == ctx.endpoint.id() || peer_id == ctx.rendezvous_id {
        return;
    }
    state.participant_endpoints.insert(author.clone(), peer_id);
    if let Err(error) = add_peer_addr(ctx.endpoint, peer_addr) {
        tracing::debug!(%error, %author, "could not seed a meta-doc endpoint into the address book");
    }
}

/// A directed leg (a directed `a2a_msg` or a `Task`) is surfaced **only by
/// its addressee** — a third party relays it without ever seeing it
/// (glossary: "a third party never sees it"). Broadcast (`to: None`) and
/// non-directed kinds are for everyone. Our own echoes never reach the
/// receive path (the pubkey self-echo gate drops them earlier), so on receive
/// "addressee" is the whole rule. This is the same gate the print/log path
/// applies in `lifecycle::{handle_msg, handle_task}`.
/// A directed frame carrying a **sealed** A2A payload (worker status/artifact
/// push, or an RPC request/response). `Pong` is directed but bodiless, so it is
/// not sealed. Sealing keys off exactly this set.
fn is_directed_content(kind: &MessageKind) -> bool {
    matches!(
        kind,
        MessageKind::A2aStatus { .. }
            | MessageKind::A2aArtifact { .. }
            | MessageKind::A2aReq { .. }
            | MessageKind::A2aResp { .. }
    )
}

/// If `message` is a directed payload sealed **to us**, decrypt its body in
/// place so the surfacing pipeline sees plaintext. Returns `false` only when a
/// frame sealed to us fails to open (tampered / wrong key), so the caller drops
/// it. A relay (not the addressee), broadcast, and plumbing pass through
/// untouched — the caller only invokes this on the addressee's path.
/// Outcome of the inbound gate. `Pass` carries the original **sealed** body to
/// restore before retention (so anti-entropy re-serves the wire frame, not our
/// decrypted view) — `Some` when we decrypted a directed frame addressed to us,
/// `None` when we did not (relay of a directed frame, or broadcast/plumbing).
enum Gated {
    Drop,
    Pass(Option<MessageBody>),
}

/// Validate + (for the addressee) decrypt an inbound **non-sharded** logical
/// frame before surfacing.
fn gate_and_decrypt(message: &mut Message, state: &EventLoopState, ctx: &HandlerCtx<'_>) -> Gated {
    if is_directed_content(&message.kind) {
        if addressed_to_us(message, ctx.author) {
            let sealed = message.body.clone();
            if !decrypt_directed(message, state, ctx) || !chat_payload_valid(message) {
                return Gated::Drop;
            }
            return Gated::Pass(Some(sealed));
        }
        // Relay: an opaque directed frame — forwarded + retained, never validated
        // or surfaced (the dispatch below is gated to the addressee).
        return Gated::Pass(None);
    }
    // Broadcast chat on a passworded swarm must arrive sealed with the swarm
    // key: decrypt in place so validation + surfacing see plaintext, rejecting
    // an unsealed or unopenable body (cleartext can't slip past the guarantee).
    // The ciphertext is returned so `ingest` restores it before retention (log +
    // anti-entropy re-serve the sealed frame). Same shape as the directed path.
    if matches!(message.kind, MessageKind::A2aMsg) && state.broadcast_key.is_some() {
        let Some(sealed) = decrypt_broadcast(message, state) else {
            return Gated::Drop;
        };
        if !chat_payload_valid(message) {
            return Gated::Drop;
        }
        return Gated::Pass(Some(sealed));
    }
    // Broadcast / other logical frames validate in place, as before.
    if chat_payload_valid(message) {
        Gated::Pass(None)
    } else {
        Gated::Drop
    }
}

/// Decrypt a broadcast chat body sealed under the swarm key, **in place**.
/// Returns the original (ciphertext) body to restore before retention, or `None`
/// when there is no key or the body is not a decryptable `enc` envelope (on a
/// passworded swarm that means an unsealed/tampered body — the caller drops it).
fn decrypt_broadcast(message: &mut Message, state: &EventLoopState) -> Option<MessageBody> {
    let key = state.broadcast_key.as_deref()?;
    let plain = crate::daemon::state_doc::decrypt_body(message.body.as_str(), Some(key))?;
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
            tracing::warn!(%error, author = %message.author, "dropping a directed frame sealed to us that failed to open");
            false
        }
    }
}

fn addressed_to_us(message: &Message, us: &Nickname) -> bool {
    match &message.kind {
        MessageKind::A2aStatus { to, .. }
        | MessageKind::A2aArtifact { to, .. }
        | MessageKind::A2aReq { to, .. }
        | MessageKind::A2aResp { to, .. } => to == us,
        // Broadcast chat is delivered to everyone.
        MessageKind::A2aMsg
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

fn maybe_push_embed(ctx: &HandlerCtx<'_>, message: &Message, surfaceable: bool) {
    if let Some(tx) = ctx.external_msg_tx
        && tx.receiver_count() > 0
        && surfaceable
        && addressed_to_us(message, ctx.author)
        && !matches!(
            message.kind,
            MessageKind::Digest
                | MessageKind::StateDigest
                | MessageKind::MetaDigest
                | MessageKind::State
                | MessageKind::Meta
                | MessageKind::Ping
                | MessageKind::Pong { .. }
                // RPC plumbing (request/response) is not chat — an embed
                // consumer must not receive it, and a burst must not lag real
                // messages out of the bounded broadcast channel.
                | MessageKind::A2aReq { .. }
                | MessageKind::A2aResp { .. }
        )
        && !is_beat_frame(message)
    {
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
    state
        .participant_endpoints
        .insert(message.author.clone(), peer_id);
    // The rendezvous is overlay plumbing, not a dialable participant peer:
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

/// The A2A boundary gate for a **logical** frame (a single wire message or a
/// reassembled shard group — never a raw shard, which carries a payload
/// fragment): a chat/task Message, a status update, or an artifact update
/// must parse and agree with its frame (id / context / addressing / role /
/// correlation — see `a2a::gossip`). Plumbing kinds pass through: their
/// bodies are transport machinery, not A2A payloads. A failing frame is
/// dropped whole — never pushed, printed, or retained — so a crafted payload
/// cannot reach the agent surface half-validated.
fn chat_payload_valid(message: &Message) -> bool {
    let outcome = match &message.kind {
        MessageKind::A2aMsg => crate::a2a::gossip::chat_payload(message).map(|_| ()),
        MessageKind::A2aStatus { .. } => crate::a2a::gossip::status_payload(message).map(|_| ()),
        MessageKind::A2aArtifact { .. } => {
            crate::a2a::gossip::artifact_payload(message).map(|_| ())
        }
        MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::A2aReq { .. }
        | MessageKind::A2aResp { .. }
        | MessageKind::State
        | MessageKind::Meta
        | MessageKind::LinkState => Ok(()),
    };
    match outcome {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                author = %message.author,
                %error,
                "dropping a2a frame with an invalid payload"
            );
            false
        }
    }
}

/// `Alive` keepalives, `PeerInfo` endpoint plumbing, anti-entropy `Digest`s,
/// and ping/pong probes are infrastructure; everything else (real chat frames
/// and `joined`/`left` presence) goes in the log (and so to `poll`/`fetch`).
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
            // A2A RPC request/response frames are plumbing (like ping/pong):
            // the result reaches the requester via its parked waiter, never
            // the chat log / poll-fetch buffer.
            | MessageKind::A2aReq { .. }
            | MessageKind::A2aResp { .. }
            // Durable state lives in its own un-pruned log, never the chat
            // message-log / poll-fetch buffer (it also returns before reaching
            // here; this keeps the predicate honest if that changes).
            | MessageKind::State
            | MessageKind::Meta
    )
}

/// Is this frame a task liveness **beat** (a status update marked
/// `swarm:beat`)? Beats are plumbing — never retained or surfaced via
/// poll/fetch, only emitted as a `task_progress` widget event. Payload-level
/// (unlike [`is_loggable`], which is kind-static), so it is checked
/// separately at the retain/push gates.
fn is_beat_frame(frame: &Message) -> bool {
    matches!(frame.kind, MessageKind::A2aStatus { .. })
        && crate::a2a::gossip::status_payload(frame)
            .is_ok_and(|payload| crate::a2a::gossip::is_beat(&payload))
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
        assert!(is_loggable(&MessageKind::A2aMsg));
        // PeerInfo is endpoint plumbing — never logged/surfaced, and the
        // classifier (not just its early-return) now says so.
        assert!(!is_loggable(&MessageKind::PeerInfo));
    }
}
