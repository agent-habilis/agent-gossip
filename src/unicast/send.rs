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
        tuning::unicast_enabled(),
        tuning::gossip_directed_enabled(),
    ) {
        Route::Gossip => broadcast(sender, bytes).await,
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
    /// No transport available: a directed message with no unicast route and
    /// gossip disabled.
    Undeliverable,
}

/// Decide the transport for `msg` under the given policy. Broadcast /
/// infrastructure kinds always take gossip; a directed message takes unicast
/// when enabled and the addressee is dialable, with gossip as the fallback
/// unless the policy forbids gossip for directed traffic.
fn route(msg: &Message, state: &EventLoopState, unicast_on: bool, gossip_on: bool) -> Route {
    let Some(nick) = sole_addressee(&msg.kind) else {
        // Broadcast / infrastructure kind: gossip is the only transport, always.
        return Route::Gossip;
    };
    match (unicast_on, unicast_endpoint(nick, state), gossip_on) {
        (true, Some(eid), true) => Route::UnicastPreferred(eid),
        (true, Some(eid), false) => Route::UnicastOnly(eid),
        // No unicast route (disabled, unknown endpoint, rendezvous, or unmeshed):
        // gossip if allowed, else the message can't be delivered.
        (_, _, true) => Route::Gossip,
        (_, _, false) => Route::Undeliverable,
    }
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
    use crate::daemon::state::EventLoopState;
    use crate::protocol::identity::Identity;
    use crate::protocol::{Message, MessageBody, Nickname, SwarmId, TaskId, TaskPhase};

    fn nick(name: &str) -> Nickname {
        Nickname::new(name.to_owned()).expect("valid test nickname")
    }

    fn endpoint_id(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn swarm() -> SwarmId {
        SwarmId::from("🐝test")
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

    fn directed_msg() -> Message {
        Message::new_reply(&swarm(), &nick("alice"), nick("bob"), body())
    }

    // ── the p2p path ──────────────────────────────────────────────────

    #[test]
    fn directed_message_with_known_endpoint_prefers_unicast() {
        let (state, bob) = state_knowing_bob();
        assert_eq!(
            route(&directed_msg(), &state, true, true),
            Route::UnicastPreferred(bob)
        );
    }

    #[test]
    fn directed_task_and_pong_also_take_unicast() {
        let (state, bob) = state_knowing_bob();
        let task = Message::new_task(
            &swarm(),
            &nick("alice"),
            nick("bob"),
            TaskId::random(),
            TaskPhase::Offer,
            body(),
        );
        let pong = Message::new_pong(&swarm(), &nick("alice"), nick("bob"));
        assert_eq!(
            route(&task, &state, true, true),
            Route::UnicastPreferred(bob)
        );
        assert_eq!(
            route(&pong, &state, true, true),
            Route::UnicastPreferred(bob)
        );
    }

    #[test]
    fn unicast_only_policy_forbids_gossip_fallback() {
        let (state, bob) = state_knowing_bob();
        assert_eq!(
            route(&directed_msg(), &state, true, false),
            Route::UnicastOnly(bob)
        );
    }

    // ── the gossip path ───────────────────────────────────────────────

    #[test]
    fn broadcast_message_always_takes_gossip() {
        let (state, _) = state_knowing_bob();
        let open = Message::new_message(&swarm(), &nick("alice"), body());
        // Even with unicast on and a known peer, a non-directed kind gossips —
        // and stays gossip regardless of policy.
        assert_eq!(route(&open, &state, true, true), Route::Gossip);
        assert_eq!(route(&open, &state, false, true), Route::Gossip);
    }

    #[test]
    fn directed_message_with_unknown_endpoint_falls_back_to_gossip() {
        let mut state = EventLoopState::new(None, Instant::now(), Arc::new(Identity::generate()));
        state.meshed = true; // meshed, but we hold no endpoint for bob
        assert_eq!(route(&directed_msg(), &state, true, true), Route::Gossip);
    }

    #[test]
    fn unmeshed_directed_message_takes_gossip() {
        let (mut state, _) = state_knowing_bob();
        state.meshed = false; // no unicast route until meshed
        assert_eq!(route(&directed_msg(), &state, true, true), Route::Gossip);
    }

    #[test]
    fn rendezvous_addressee_is_never_a_unicast_target() {
        let mut state = EventLoopState::new(None, Instant::now(), Arc::new(Identity::generate()));
        state.meshed = true;
        let rendezvous = endpoint_id(9);
        state.rendezvous_id = Some(rendezvous);
        // bob's advertised endpoint *is* the rendezvous pseudo-node.
        state.participant_endpoints.insert(nick("bob"), rendezvous);
        assert_eq!(route(&directed_msg(), &state, true, true), Route::Gossip);
    }

    #[test]
    fn disabling_unicast_routes_directed_to_gossip() {
        let (state, _) = state_knowing_bob();
        // Unicast off + a known peer ⇒ still gossip (the `--no-unicast` switch).
        assert_eq!(route(&directed_msg(), &state, false, true), Route::Gossip);
    }

    // ── undeliverable ─────────────────────────────────────────────────

    #[test]
    fn unicast_only_with_unknown_endpoint_is_undeliverable() {
        let mut state = EventLoopState::new(None, Instant::now(), Arc::new(Identity::generate()));
        state.meshed = true; // no endpoint for bob, and gossip forbidden
        assert_eq!(
            route(&directed_msg(), &state, true, false),
            Route::Undeliverable
        );
    }

    #[test]
    fn both_transports_disabled_is_undeliverable_for_directed() {
        let (state, _) = state_knowing_bob();
        assert_eq!(
            route(&directed_msg(), &state, false, false),
            Route::Undeliverable
        );
    }

    #[test]
    fn broadcast_survives_both_transports_disabled() {
        // A non-directed kind never hits the undeliverable arm — gossip is
        // structural for broadcasts, not subject to the directed-gossip switch.
        let (state, _) = state_knowing_bob();
        let open = Message::new_message(&swarm(), &nick("alice"), body());
        assert_eq!(route(&open, &state, false, false), Route::Gossip);
    }
}
