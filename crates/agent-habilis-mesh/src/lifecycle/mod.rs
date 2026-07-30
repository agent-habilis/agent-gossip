//! The **lifecycle subsystem**: everything about a *peer's*
//! presence over time — heartbeat + membership transitions + the
//! join-horizon surfacing decisions (`joined` / `left` / `peer_return`
//! / `peer_timeout`). The pure gossip router calls [`observe`] for
//! every received message, then dispatches by kind into the
//! presence/msg handlers here. The gossip layer never touches the
//! roster directly — that separation is the layer split documented in
//! the Concept Glossary (AGENTS.md).

pub(crate) mod heartbeat;
pub(crate) mod membership;

use std::time::Instant;

use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::gossip::app::NodeApp;
use crate::gossip::event::NodeEvent;
use crate::protocol::{Message, MessageKind, PresenceSubtype};

use crate::gossip;

/// Developer log for the mesh-ready milestone. Mirrors the operator
/// `ready` event but on the lifecycle log target (stderr, opt-in via
/// `RUST_LOG`); the operator JSON/human event is unchanged.
///
/// Logs the derived **`TopicId`** (a one-way hash of the seed), never the full
/// `💬…` id. The id carries the seed and *is* the bearer credential, and this
/// log file is written under a shared path — logging the id would leak full
/// mesh membership to anyone who can read the file. The topic hash is enough
/// to correlate a run without exposing the secret.
pub(crate) fn log_ready(
    topic: iroh_gossip::proto::TopicId,
    name: &str,
    nickname: &str,
    network: &str,
) {
    tracing::info!(target: "agent_habilis_mesh::lifecycle", ?topic, name, nickname, network, "mesh ready");
}

/// Developer log for graceful departure (mirrors the operator `left`).
pub(crate) fn log_leaving(name: &str) {
    tracing::info!(target: "agent_habilis_mesh::lifecycle", name, "leaving mesh");
}

/// What [`observe`] computed for one received message: whether it is
/// past our join horizon (surfaceable) and how it changed the roster.
/// Returned to the gossip router, which needs both for the inbound-push
/// gate and the presence/msg dispatch.
pub(crate) struct Observed {
    pub surfaceable: bool,
    pub update: membership::MembershipUpdate,
}

/// Heartbeat + membership + surfacing + join-horizon for one received
/// message. Called by the gossip router *before* kind dispatch. All
/// membership/presentation side effects live here (lifecycle layer);
/// the gossip layer never touches the roster directly.
pub(crate) fn observe(
    message: &Message,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) -> Observed {
    // Implicit heartbeat: any received message updates last_seen.
    match state.last_seen.get_mut(message.author.as_str()) {
        Some(seen) => *seen = Instant::now(),
        None => {
            state
                .last_seen
                .insert(message.author.clone(), Instant::now());
        }
    }

    let update = membership::compute(&message.kind, &message.author, state);
    membership::apply(&update, &message.author, state);

    // Join horizon: the node still relays/logs everything (anti-entropy
    // keeps the mesh's set uniform), but a message stamped before we
    // joined is never *surfaced* to the operator/agent. Computed here
    // (not just before the inbound push) because the arrival-surfacing
    // decisions below need it too.
    let surfaceable = message.timestamp >= state.joined_at;

    if update.returned && surfaceable {
        state.surfaced.insert(message.author.clone());
        ctx.sink.emit(NodeEvent::PeerReturn {
            nickname: message.author.clone(),
        });
        tracing::info!(target: "agent_habilis_mesh::lifecycle", nickname = %message.author, "peer returned");
    }

    // Make `joined` as reliable as membership. A peer's own `joined`
    // presence is a one-shot broadcast that can be lost in the
    // convergence window; `PeerInfo` is re-sent on every `NeighborUp`
    // and flooded by receivers, so a peer first learned via *any*
    // non-presence message reliably surfaces a `joined` here. The
    // real `Presence::Joined` keeps its own emit + roster re-announce
    // in `handle_presence`; gating on `!Presence` avoids a double.
    //
    // Surfacing is gated on `surfaceable` + `surfaced`, NOT on
    // `joined_new`: `joined_new` (the roster concept) is consumed by
    // the *first* received message even when it is non-surfaceable
    // pre-join backlog, so keying off it would swallow the legitimate
    // arrival of a still-present peer whose first fresh message lands
    // after some old relayed one. `!surfaced.contains` keeps
    // this exactly-once instead.
    if surfaceable
        && !update.returned
        && !state.surfaced.contains(message.author.as_str())
        && !matches!(message.kind, MessageKind::Presence { .. })
    {
        state.surfaced.insert(message.author.clone());
        ctx.sink.emit(NodeEvent::Presence {
            msg: Box::new(Message::new_joined(ctx.mesh, &message.author)),
        });
        tracing::info!(target: "agent_habilis_mesh::lifecycle", nickname = %message.author, "peer joined");
    }

    Observed {
        surfaceable,
        update,
    }
}

/// A received `Presence` frame plus the roster delta [`membership::observe`]
/// already computed for it, passed to [`handle_presence`]. Constructed by
/// [`crate::gossip::recv::ingest`], across the `gossip`/`lifecycle` module
/// boundary — hence `pub(crate)` rather than module-private.
pub(crate) struct PresenceEvent<'a> {
    pub(crate) message: &'a Message,
    pub(crate) subtype: PresenceSubtype,
    pub(crate) update: &'a membership::MembershipUpdate,
    pub(crate) surfaceable: bool,
}

pub(crate) async fn handle_presence(
    event: PresenceEvent<'_>,
    state: &mut EventLoopState,
    app: &mut dyn NodeApp,
    ctx: &HandlerCtx<'_>,
) {
    let PresenceEvent {
        message,
        subtype,
        update,
        surfaceable,
    } = event;
    if subtype == PresenceSubtype::Alive {
        return;
    }
    if subtype == PresenceSubtype::Left {
        if state.peers.remove(message.author.as_str()) {
            state.write_peer_count();
        }
        // A graceful goodbye, unlike a silence timeout, drops the dial hint —
        // and with it the warm unicast connection and any dial cooldown, so a
        // rejoin re-dials cold.
        if let Some(endpoint_id) = state.peer_endpoints.remove(message.author.as_str()) {
            state.unicast_pool.forget(endpoint_id).await;
        }
        state.quiet.remove(message.author.as_str());
        // Only announce a departure for a peer whose arrival we
        // surfaced — keeps the join-horizon view symmetric. A `left`
        // for a peer known only through pre-join backlog (or one we
        // already showed going quiet) is dropped.
        if state.surfaced.remove(message.author.as_str()) {
            ctx.sink.emit(NodeEvent::Presence {
                msg: Box::new(message.clone()),
            });
            tracing::info!(target: "agent_habilis_mesh::lifecycle", nickname = %message.author, "peer left");
        }
        // App cleanup last, with the roster already consistent. Gated on
        // `surfaceable`: a relayed pre-join `Left` must never cancel live
        // work — the same join-horizon symmetry as the announcement above.
        if surfaceable {
            app.on_peer_left(&message.author, state, ctx).await;
        }
    } else if subtype == PresenceSubtype::Joined && update.joined_new {
        // Re-announce so late joiners seed their roster.
        gossip::broadcast_msg(
            ctx.sender,
            &Message::new_joined(ctx.mesh, ctx.author).signed(ctx.identity),
        )
        .await;
        state.last_sent_at = Instant::now();
        // Suppress "has joined" when we already printed "came back"
        // from the quiet check, or when this `joined` predates
        // our own join (relayed backlog).
        if surfaceable && !update.returned {
            state.surfaced.insert(message.author.clone());
            ctx.sink.emit(NodeEvent::Presence {
                msg: Box::new(message.clone()),
            });
            tracing::info!(target: "agent_habilis_mesh::lifecycle", nickname = %message.author, "peer joined (announced)");
        }
    }
}
