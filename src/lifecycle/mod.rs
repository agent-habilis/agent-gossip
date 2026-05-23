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
pub(crate) fn log_ready(swarm: &str, name: &str, nickname: &str, network: &str) {
    tracing::info!(swarm, name, nickname, network, "swarm ready");
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
        ctx.output.peer_return(message.author.as_str());
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
        gossip::broadcast_msg(ctx.sender, &Message::new_joined(ctx.swarm, ctx.author)).await;
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

/// Returns whether the message should be logged (pushed to the poll
/// buffer). A reply addressed to another peer is neither logged nor
/// shown here. `surfaceable` gates only the *display*: a pre-join
/// message is still logged/relayed but not printed.
pub(crate) fn handle_msg(
    out: &output::Output,
    message: &Message,
    surfaceable: bool,
    self_author: &Nickname,
) -> bool {
    match &message.kind {
        MessageKind::Msg { reply: None } => {
            if surfaceable {
                out.print_message(message);
            }
            true
        }
        MessageKind::Msg {
            reply: Some(target),
        } => {
            if target != self_author {
                return false;
            }
            if surfaceable {
                out.print_message_ex(message, false);
            }
            true
        }
        MessageKind::Presence { .. } | MessageKind::PeerInfo | MessageKind::Digest => false,
    }
}
