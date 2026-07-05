//! The single send decision. Every directed message funnels through [`deliver`],
//! which picks the transport: a sole-addressee message we can dial goes over the
//! unicast connection; a broadcast, or an addressee we can't reach by unicast,
//! goes over gossip. The bytes are the same canonical wire `Message` gossip
//! would carry, so the receiver's path is identical either way.

use anyhow::Result;
use bytes::Bytes;
use iroh::EndpointId;
use iroh_gossip::api::GossipSender;

use crate::daemon::state::EventLoopState;
use crate::protocol::message::sole_addressee;
use crate::protocol::{Message, Nickname};
use crate::util::tuning;

/// Route one already-signed, already-serialized message. `bytes` MUST be
/// `msg.serialize()` — the same wire form gossip uses.
///
/// - A message with no sole addressee (a broadcast or infrastructure kind) is
///   always gossip-broadcast.
/// - A directed message is sent over a warm unicast connection when one exists
///   (and returns without touching gossip — this is what stops the flood); if
///   the connection is cold it is warmed in the background and this message
///   rides gossip. Under unicast-only policy there is no gossip fallback, so a
///   cold peer is dialed inline instead.
///
/// # Errors
/// Propagates a gossip broadcast failure, an inline unicast dial/send failure
/// (unicast-only policy), or reports a directed message undeliverable when both
/// transports are disabled.
pub(crate) async fn deliver(
    msg: &Message,
    bytes: Bytes,
    state: &EventLoopState,
    sender: &GossipSender,
) -> Result<()> {
    // The transport decision is pure (see `route`); this function just runs it.
    match route(
        msg,
        state,
        state.transport.unicast,
        state.transport.gossip_directed,
        state.transport.whisper,
    ) {
        Route::Gossip => broadcast(sender, bytes).await,
        Route::Whisper(target) => deliver_over_whisper(target, bytes, state, sender).await,
        Route::UnicastPreferred(eid) => {
            let pool = state.unicast_pool.clone();
            if pool.send_if_warm(eid, bytes.clone()).await {
                // Sent point-to-point; the flood is avoided.
                Ok(())
            } else {
                // Cold: warm for next time, ride gossip now.
                pool.warm(eid);
                broadcast(sender, bytes).await
            }
        }
        Route::UnicastOnly(eid) => {
            let pool = state.unicast_pool.clone();
            if pool.send_if_warm(eid, bytes.clone()).await {
                Ok(())
            } else {
                // No gossip fallback under this policy — dial inline; a failure
                // surfaces rather than silently flooding.
                pool.dial_and_send(eid, bytes).await
            }
        }
        Route::Undeliverable => Err(anyhow::anyhow!(
            "directed message undeliverable: no unicast route and gossip is disabled"
        )),
    }
}

/// The chosen transport for a message, given the policy — a **pure** decision so
/// both paths (p2p / gossip) are unit-testable without touching the network.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// Broadcast over gossip — a non-directed kind, or a directed message with no
    /// unicast route while gossip is allowed.
    Gossip,
    /// Prefer unicast to this endpoint; fall back to gossip if it isn't warm.
    UnicastPreferred(EndpointId),
    /// Unicast to this endpoint only — no gossip fallback (dial inline if cold).
    UnicastOnly(EndpointId),
    /// A directed message to a peer we know (`EndpointId`) but have no *direct*
    /// unicast route to: route it over a multi-hop **whisper** circuit. `deliver`
    /// computes the source-route from the link-state graph and falls back to
    /// gossip if there is no path (or the circuit fails) — unless directed
    /// gossip is disabled, which surfaces the failure instead.
    Whisper(EndpointId),
    /// No transport available: a directed message with no unicast route and
    /// gossip disabled.
    Undeliverable,
}

/// Decide the transport for `msg` under the given policy. Broadcast /
/// infrastructure kinds always take gossip; a directed message takes unicast
/// when enabled and the addressee is dialable, with gossip as the fallback
/// unless the policy forbids gossip for directed traffic.
fn route(
    msg: &Message,
    state: &EventLoopState,
    unicast_on: bool,
    gossip_on: bool,
    relay_on: bool,
) -> Route {
    let Some(nick) = sole_addressee(&msg.kind) else {
        // Broadcast / infrastructure kind: gossip is the only transport, always.
        return Route::Gossip;
    };
    directed_route(nick, state, unicast_on, gossip_on, relay_on)
}

/// The message-independent half of [`route`]: every directed kind to the same
/// addressee routes identically, so this is also what the `peers` roster
/// surfaces as a peer's `transport` column (via [`lane_for`]).
fn directed_route(
    nick: &Nickname,
    state: &EventLoopState,
    unicast_on: bool,
    gossip_on: bool,
    relay_on: bool,
) -> Route {
    if let (true, Some(eid)) = (unicast_on, unicast_endpoint(nick, state)) {
        return if gossip_on {
            Route::UnicastPreferred(eid)
        } else {
            Route::UnicastOnly(eid)
        };
    }
    // No *direct* unicast route. If we know the peer's endpoint, prefer the relay
    // circuit transport (it can route to a known peer through the mesh); else
    // gossip if allowed; else the message can't be delivered.
    if relay_on && let Some(target) = target_endpoint(nick, state) {
        return Route::Whisper(target);
    }
    if gossip_on {
        Route::Gossip
    } else {
        Route::Undeliverable
    }
}

/// The lane a directed frame to `nick` would take right now, under the
/// session's live policy — [`Route`] stripped of its endpoint payloads so the
/// roster can serialize it. Sharing `directed_route` keeps the surfaced
/// column from ever drifting from the real send decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Lane {
    Unicast,
    Whisper,
    Gossip,
    Unreachable,
}

pub(crate) fn lane_for(nick: &Nickname, state: &EventLoopState) -> Lane {
    match directed_route(
        nick,
        state,
        state.transport.unicast,
        state.transport.gossip_directed,
        state.transport.whisper,
    ) {
        Route::UnicastPreferred(_) | Route::UnicastOnly(_) => Lane::Unicast,
        Route::Whisper(_) => Lane::Whisper,
        Route::Gossip => Lane::Gossip,
        Route::Undeliverable => Lane::Unreachable,
    }
}

/// The endpoint we hold for `nick` regardless of whether it is *directly*
/// dialable — the relay transport can route to a known peer through the mesh
/// even when a direct unicast route is unavailable. Excludes the rendezvous
/// pseudo-node (never a relay destination).
fn target_endpoint(nick: &Nickname, state: &EventLoopState) -> Option<EndpointId> {
    let eid = *state.participant_endpoints.get(nick)?;
    if state.rendezvous_id == Some(eid) {
        return None;
    }
    Some(eid)
}

/// The endpoint a directed message to `nick` can be unicast to, or `None` when
/// there is no unicast route: an addressee we hold no endpoint for, the
/// rendezvous pseudo-node (never a unicast target), or an unmeshed node.
fn unicast_endpoint(nick: &Nickname, state: &EventLoopState) -> Option<EndpointId> {
    if !state.meshed {
        return None;
    }
    let eid = *state.participant_endpoints.get(nick)?;
    if state.rendezvous_id == Some(eid) {
        return None;
    }
    Some(eid)
}

/// Deliver a directed message to a known-but-not-directly-reachable peer over a
/// multi-hop relay circuit: compute the source-route from the link-state graph
/// and telescope a circuit to it. When we have no endpoint, no graph path (or a
/// hop's key is unknown), or every circuit build fails, falls back to gossip —
/// unless the policy forbids gossip for directed traffic, which surfaces the
/// failure instead (so a whisper-only run can't silently ride gossip).
async fn deliver_over_whisper(
    target: EndpointId,
    bytes: Bytes,
    state: &EventLoopState,
    sender: &GossipSender,
) -> Result<()> {
    let Some(endpoint) = state.unicast_pool.endpoint() else {
        return whisper_fallback(sender, bytes, state, "detached unicast pool").await;
    };
    // Up to N node-disjoint circuits, best-first — the "up to N tries" budget.
    let paths = state
        .link_state
        .circuit_paths(endpoint.id(), target, tuning::WHISPER_MAX_PATHS);
    if paths.is_empty() {
        // No route through the mesh yet (or a hop hasn't advertised its key).
        return whisper_fallback(sender, bytes, state, "no circuit path through the mesh").await;
    }
    for path in &paths {
        let circuit_id: u64 = rand::random();
        match crate::whisper::open_circuit(&endpoint, circuit_id, path, bytes.as_ref()).await {
            Ok(()) => return Ok(()),
            Err(error) => tracing::debug!(
                target: crate::whisper::LOG_TARGET,
                %error,
                "relay circuit attempt failed; trying the next disjoint path"
            ),
        }
    }
    whisper_fallback(sender, bytes, state, "all relay circuits failed").await
}

/// The whisper plane could not deliver: ride gossip when the policy allows it,
/// else surface the failure so the caller (and its waiter timeout) sees it.
async fn whisper_fallback(
    sender: &GossipSender,
    bytes: Bytes,
    state: &EventLoopState,
    reason: &str,
) -> Result<()> {
    if state.transport.gossip_directed {
        tracing::debug!(
            target: crate::whisper::LOG_TARGET,
            reason,
            "whisper undeliverable; falling back to gossip"
        );
        return broadcast(sender, bytes).await;
    }
    Err(anyhow::anyhow!(
        "directed message undeliverable over whisper ({reason}) and gossip is disabled"
    ))
}

async fn broadcast(sender: &GossipSender, bytes: Bytes) -> Result<()> {
    sender
        .broadcast(bytes)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use iroh::{EndpointId, SecretKey};

    use super::{Route, route};
    use crate::a2a::A2aRpcId;
    use crate::daemon::state::EventLoopState;
    use crate::protocol::identity::Identity;
    use crate::protocol::{Message, MessageBody, Nickname, SwarmId};

    fn nick(name: &str) -> Nickname {
        Nickname::from(name)
    }

    fn endpoint_id(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn swarm() -> SwarmId {
        SwarmId::from("💬test")
    }

    fn body() -> MessageBody {
        MessageBody::from("hi")
    }

    /// A meshed state that knows `bob`'s endpoint — the happy path for unicast.
    fn state_knowing_bob() -> (EventLoopState, EndpointId) {
        let mut state = EventLoopState::new(None, Instant::now(), Arc::new(Identity::generate()));
        state.meshed = true;
        let bob = endpoint_id(1);
        state.participant_endpoints.insert(nick("bob"), bob);
        (state, bob)
    }

    /// A directed frame addressed to `bob` — a `Pong` is the simplest one.
    fn directed_msg() -> Message {
        Message::new_pong(&swarm(), &nick("alice"), nick("bob"))
    }

    // Existing cases pin the direct-unicast/gossip behavior with the relay
    // transport OFF, so they assert the pre-relay routing unchanged.

    // ── the p2p path ──────────────────────────────────────────────────

    #[test]
    fn directed_message_with_known_endpoint_prefers_unicast() {
        let (state, bob) = state_knowing_bob();
        assert_eq!(
            route(&directed_msg(), &state, true, true, false),
            Route::UnicastPreferred(bob)
        );
    }

    #[test]
    fn directed_rpc_and_pong_also_take_unicast() {
        let (state, bob) = state_knowing_bob();
        let req = Message::new_a2a_req(
            &swarm(),
            &nick("alice"),
            nick("bob"),
            A2aRpcId::random(),
            body(),
        );
        let pong = Message::new_pong(&swarm(), &nick("alice"), nick("bob"));
        assert_eq!(
            route(&req, &state, true, true, false),
            Route::UnicastPreferred(bob)
        );
        assert_eq!(
            route(&pong, &state, true, true, false),
            Route::UnicastPreferred(bob)
        );
    }

    #[test]
    fn unicast_only_policy_forbids_gossip_fallback() {
        let (state, bob) = state_knowing_bob();
        assert_eq!(
            route(&directed_msg(), &state, true, false, false),
            Route::UnicastOnly(bob)
        );
    }

    // ── the gossip path ───────────────────────────────────────────────

    #[test]
    fn broadcast_message_always_takes_gossip() {
        let (state, _) = state_knowing_bob();
        let open = Message::new_a2a_msg(&swarm(), &nick("alice"), body());
        // Even with unicast on and a known peer, a non-directed kind gossips —
        // and stays gossip regardless of policy.
        assert_eq!(route(&open, &state, true, true, false), Route::Gossip);
        assert_eq!(route(&open, &state, false, true, false), Route::Gossip);
    }

    #[test]
    fn directed_message_with_unknown_endpoint_falls_back_to_gossip() {
        let mut state = EventLoopState::new(None, Instant::now(), Arc::new(Identity::generate()));
        state.meshed = true; // meshed, but we hold no endpoint for bob
        assert_eq!(
            route(&directed_msg(), &state, true, true, false),
            Route::Gossip
        );
    }

    #[test]
    fn unmeshed_directed_message_takes_gossip() {
        let (mut state, _) = state_knowing_bob();
        state.meshed = false; // no unicast route until meshed
        assert_eq!(
            route(&directed_msg(), &state, true, true, false),
            Route::Gossip
        );
    }

    #[test]
    fn rendezvous_addressee_is_never_a_unicast_target() {
        let mut state = EventLoopState::new(None, Instant::now(), Arc::new(Identity::generate()));
        state.meshed = true;
        let rendezvous = endpoint_id(9);
        state.rendezvous_id = Some(rendezvous);
        // bob's advertised endpoint *is* the rendezvous pseudo-node.
        state.participant_endpoints.insert(nick("bob"), rendezvous);
        assert_eq!(
            route(&directed_msg(), &state, true, true, false),
            Route::Gossip
        );
    }

    #[test]
    fn disabling_unicast_routes_directed_to_gossip() {
        let (state, _) = state_knowing_bob();
        // Unicast off + a known peer ⇒ still gossip (the `--no-unicast` switch).
        assert_eq!(
            route(&directed_msg(), &state, false, true, false),
            Route::Gossip
        );
    }

    // ── undeliverable ─────────────────────────────────────────────────

    #[test]
    fn unicast_only_with_unknown_endpoint_is_undeliverable() {
        let mut state = EventLoopState::new(None, Instant::now(), Arc::new(Identity::generate()));
        state.meshed = true; // no endpoint for bob, and gossip forbidden
        assert_eq!(
            route(&directed_msg(), &state, true, false, false),
            Route::Undeliverable
        );
    }

    #[test]
    fn both_transports_disabled_is_undeliverable_for_directed() {
        let (state, _) = state_knowing_bob();
        assert_eq!(
            route(&directed_msg(), &state, false, false, false),
            Route::Undeliverable
        );
    }

    #[test]
    fn broadcast_survives_both_transports_disabled() {
        // A non-directed kind never hits the undeliverable arm — gossip is
        // structural for broadcasts, not subject to the directed-gossip switch.
        let (state, _) = state_knowing_bob();
        let open = Message::new_a2a_msg(&swarm(), &nick("alice"), body());
        assert_eq!(route(&open, &state, false, false, false), Route::Gossip);
    }

    // ── the relay tier (Phase 1: stub, tier ordering only) ─────────────

    #[test]
    fn known_peer_without_direct_route_prefers_relay() {
        // Bob's endpoint is known but we're unmeshed ⇒ no direct unicast route ⇒
        // route to him over a relay circuit rather than a gossip flood.
        let (mut state, bob) = state_knowing_bob();
        state.meshed = false;
        assert_eq!(
            route(&directed_msg(), &state, true, true, true),
            Route::Whisper(bob)
        );
    }

    #[test]
    fn direct_unicast_route_beats_relay() {
        // A directly-dialable peer still goes unicast — relay is only for the
        // no-direct-route case.
        let (state, bob) = state_knowing_bob();
        assert_eq!(
            route(&directed_msg(), &state, true, true, true),
            Route::UnicastPreferred(bob)
        );
    }

    #[test]
    fn unknown_peer_cannot_relay_and_falls_back_to_gossip() {
        // No endpoint for the target ⇒ nothing to route a circuit to ⇒ gossip,
        // even with the relay transport enabled.
        let mut state = EventLoopState::new(None, Instant::now(), Arc::new(Identity::generate()));
        state.meshed = true;
        assert_eq!(
            route(&directed_msg(), &state, true, true, true),
            Route::Gossip
        );
    }

    #[test]
    fn relay_disabled_falls_back_to_gossip() {
        // `--no-relay` with no direct route ⇒ the gossip fallback.
        let (mut state, _) = state_knowing_bob();
        state.meshed = false;
        assert_eq!(
            route(&directed_msg(), &state, true, true, false),
            Route::Gossip
        );
    }
}
