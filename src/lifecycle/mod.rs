//! The **lifecycle subsystem**: everything about a *participant's*
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
use crate::output;
use crate::protocol::{Message, MessageKind, Nickname, PresenceSubtype};

use crate::gossip;

/// Developer log for the swarm-ready milestone. Mirrors the operator
/// `ready` event but on the lifecycle log target (stderr, opt-in via
/// `RUST_LOG`); the operator JSON/human event is unchanged.
///
/// Logs the derived **`TopicId`** (a one-way hash of the seed), never the full
/// `💬…` id. The id carries the seed and *is* the bearer credential, and this
/// log file is written under a shared path — logging the id would leak full
/// swarm membership to anyone who can read the file. The topic hash is enough
/// to correlate a run without exposing the secret.
pub(crate) fn log_ready(
    topic: iroh_gossip::proto::TopicId,
    name: &str,
    nickname: &str,
    network: &str,
) {
    tracing::info!(?topic, name, nickname, network, "swarm ready");
}

/// Developer log for graceful departure (mirrors the operator `left`).
pub(crate) fn log_leaving(name: &str) {
    tracing::info!(name, "leaving swarm");
}

/// What [`observe`] computed for one received message: whether it is
/// past our join horizon (surfaceable) and how it changed the roster.
/// Returned to the gossip router, which needs both for the embed-push
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
    // keeps the swarm's set uniform), but a message stamped before we
    // joined is never *surfaced* to the operator/agent. Computed here
    // (not just before the embed push) because the arrival-surfacing
    // decisions below need it too.
    let surfaceable = message.timestamp >= state.joined_at;

    if update.returned && surfaceable {
        state.surfaced.insert(message.author.clone());
        ctx.output.peer_return(&message.author);
        tracing::info!(nickname = %message.author, "peer returned");
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
        ctx.output
            .print_presence(&Message::new_joined(ctx.swarm, &message.author));
        tracing::info!(nickname = %message.author, "peer joined");
    }

    Observed {
        surfaceable,
        update,
    }
}

pub(crate) async fn handle_presence(
    message: &Message,
    subtype: PresenceSubtype,
    update: &membership::MembershipUpdate,
    surfaceable: bool,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    if subtype == PresenceSubtype::Alive {
        return;
    }
    if subtype == PresenceSubtype::Left {
        if state.participants.remove(message.author.as_str()) {
            state.write_participant_count();
        }
        state.participant_endpoints.remove(message.author.as_str());
        state.quiet.remove(message.author.as_str());
        // Only announce a departure for a peer whose arrival we
        // surfaced — keeps the join-horizon view symmetric. A `left`
        // for a peer known only through pre-join backlog (or one we
        // already showed going quiet) is dropped.
        if state.surfaced.remove(message.author.as_str()) {
            ctx.output.print_presence(message);
            tracing::info!(nickname = %message.author, "peer left");
        }
    } else if subtype == PresenceSubtype::Joined && update.joined_new {
        // Re-announce so late joiners seed their roster.
        gossip::broadcast_msg(
            ctx.sender,
            &Message::new_joined(ctx.swarm, ctx.author).signed(ctx.identity),
        )
        .await;
        state.last_sent_at = Instant::now();
        // Suppress "has joined" when we already printed "came back"
        // from the quiet check, or when this `joined` predates
        // our own join (relayed backlog).
        if surfaceable && !update.returned {
            state.surfaced.insert(message.author.clone());
            ctx.output.print_presence(message);
            tracing::info!(nickname = %message.author, "peer joined (announced)");
        }
    }
}

/// Returns whether the message should be logged (pushed to the poll buffer).
/// `A2aMsg` is swarm broadcast chat — always loggable. `surfaceable` gates
/// only the *display*: a pre-join message is still logged/relayed but not
/// printed.
pub(crate) fn handle_msg(out: &output::Output, message: &Message, surfaceable: bool) -> bool {
    match &message.kind {
        MessageKind::A2aMsg => {
            if surfaceable {
                out.print_message(message);
            }
            true
        }
        MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::State
        | MessageKind::Meta
        | MessageKind::A2aStatus { .. }
        | MessageKind::A2aArtifact { .. }
        | MessageKind::A2aReq { .. }
        | MessageKind::A2aResp { .. }
        | MessageKind::LinkState => false,
    }
}

/// A task leg (a task-related `a2a_msg`, an `a2a_status`, or an
/// `a2a_artifact`): surfaced + logged only by the addressee (`to ==
/// self_author`) and, via the sender's echo path, the sender itself —
/// third parties relay it without retaining, exactly like a directed
/// message (see [`handle_msg`]). Returns whether to **log** (content legs
/// only — a beat status is liveness plumbing, surfaced as a `task_progress`
/// widget event but never retained). `surfaceable` gates only the *display*
/// (join-horizon), never the relay/log.
pub(crate) fn handle_task(
    out: &output::Output,
    message: &Message,
    to: &Nickname,
    surfaceable: bool,
    self_author: &Nickname,
) -> bool {
    if to != self_author {
        return false;
    }
    if surfaceable {
        out.print_task(message, false);
    }
    // Content legs log; a liveness beat (a status marked `swarm:beat`) is
    // plumbing — surfaced as a `task_progress` widget event but never retained.
    !crate::a2a::gossip::status_payload(message)
        .is_ok_and(|payload| crate::a2a::gossip::is_beat(&payload))
}
