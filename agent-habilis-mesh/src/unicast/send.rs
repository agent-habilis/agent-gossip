//! The single send decision. Every directed message funnels through [`deliver`],
//! which picks the transport: a sole-addressee message we can dial goes over the
//! unicast connection; a broadcast, or an addressee we can't reach by unicast,
//! goes over gossip. The bytes are the same canonical wire `Message` gossip
//! would carry, so the receiver's path is identical either way.

use anyhow::Result;
use bytes::Bytes;
use iroh::EndpointId;

use crate::daemon::state::EventLoopState;
use crate::protocol::message::sole_addressee;
use crate::protocol::{Message, Nickname};
use crate::transport::MeshSender;

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
/// (unicast-only policy), or reports the directed message undeliverable when its
/// chosen transport fell through and the policy forbids a gossip fallback.
pub async fn deliver(
    msg: &Message,
    bytes: Bytes,
    state: &EventLoopState,
    sender: &MeshSender,
) -> Result<()> {
    // Per-session transport policy drives the pure `route` decision; this
    // function runs the chosen directed transport, then applies the **one**
    // gossip fallback (no arm falls back on its own — Part B of the refactor).
    let policy = state.transport;
    let attempt = match route(msg, state, policy) {
        // Gossip is the transport itself here, not a fallback.
        Route::Gossip => return broadcast(sender, bytes).await,
        Route::Undeliverable => {
            return Err(anyhow::anyhow!(
                "directed message undeliverable: no unicast route and gossip is disabled"
            ));
        }
        Route::UnicastPreferred(eid) => try_unicast_warm(eid, &bytes, state, sender).await,
        Route::UnicastOnly(eid) => try_unicast_only(eid, &bytes, state, sender).await,
    };
    match attempt {
        Attempt::Delivered => Ok(()),
        // The single, central fallback: ride gossip iff the policy permits
        // gossip for directed traffic; otherwise the frame is undeliverable.
        Attempt::FellThrough if policy.gossip_directed => broadcast(sender, bytes).await,
        Attempt::FellThrough => Err(anyhow::anyhow!(
            "directed message undeliverable: the directed transport failed and gossip is disabled"
        )),
    }
}

/// Outcome of one directed-transport attempt. A helper never falls back itself —
/// `FellThrough` hands the decision to [`deliver`]'s single gossip fallback.
enum Attempt {
    Delivered,
    FellThrough,
}

/// The chosen transport for a message, given the policy — a **pure** decision so
/// both paths (p2p / gossip) are unit-testable without touching the network.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// Broadcast over gossip — a non-directed kind, or a directed message with no
    /// unicast route while gossip is allowed.
    Gossip,
    /// Prefer unicast to this endpoint; fall back to gossip if it isn't warm. The
    /// underlying iroh connection uses a direct path when one exists, else the
    /// registered multihop transport — the send decision no longer distinguishes.
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
fn route(
    msg: &Message,
    state: &EventLoopState,
    policy: crate::transport::TransportPolicy,
) -> Route {
    let Some(nick) = sole_addressee(&msg.kind) else {
        // Broadcast / infrastructure kind: gossip is the only transport, always.
        return Route::Gossip;
    };
    directed_route(nick, state, policy)
}

/// The message-independent half of [`route`]: every directed kind to the same
/// addressee routes identically, so this is also what the `peers` roster
/// surfaces as a peer's `transport` column (via [`lane_for`]).
///
/// A peer whose endpoint we know is reached by a unicast connect — iroh's path
/// selection uses a direct path when one exists and the registered multihop
/// transport otherwise, so there is no separate multi-hop tier in the send
/// decision anymore.
fn directed_route(
    nick: &Nickname,
    state: &EventLoopState,
    policy: crate::transport::TransportPolicy,
) -> Route {
    if let (true, Some(eid)) = (policy.unicast, directed_endpoint(nick, state)) {
        return if policy.gossip_directed {
            Route::UnicastPreferred(eid)
        } else {
            Route::UnicastOnly(eid)
        };
    }
    if policy.gossip_directed {
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
pub enum Lane {
    Unicast,
    Multihop,
    Gossip,
    Unreachable,
}

/// The roster's `transport` column. A directed frame to a known peer takes a
/// unicast connect; we label it `unicast` when a *direct* path is expected (the
/// peer is in our gossip mesh) and `multihop` otherwise — a best-effort hint,
/// since the actual path is chosen by iroh at connect time.
pub(crate) fn lane_for(nick: &Nickname, state: &EventLoopState) -> Lane {
    match directed_route(nick, state, state.transport) {
        Route::UnicastPreferred(_) | Route::UnicastOnly(_) => {
            if directly_meshed(nick, state) {
                Lane::Unicast
            } else {
                Lane::Multihop
            }
        }
        Route::Gossip => Lane::Gossip,
        Route::Undeliverable => Lane::Unreachable,
    }
}

/// The endpoint a directed message to `nick` can be sent to, or `None` for an
/// addressee we hold no endpoint for or the rendezvous pseudo-node (never a
/// directed target). Reachability is left to iroh's connect (direct or multihop).
fn directed_endpoint(nick: &Nickname, state: &EventLoopState) -> Option<EndpointId> {
    let eid = *state.participant_endpoints.get(nick)?;
    if state.rendezvous_id == Some(eid) {
        return None;
    }
    Some(eid)
}

/// Whether `nick` is a known peer in our live gossip mesh — the signal that a
/// *direct* unicast path is expected rather than a multihop one.
fn directly_meshed(nick: &Nickname, state: &EventLoopState) -> bool {
    state.meshed && directed_endpoint(nick, state).is_some()
}

/// Warm-preferred unicast: send point-to-point when the connection is already
/// warm, else warm it in the background and fall through (never blocking on a
/// cold dial). Mirrors to the spool on its own success — it bypassed
/// `broadcast`'s tee.
async fn try_unicast_warm(
    eid: EndpointId,
    bytes: &Bytes,
    state: &EventLoopState,
    sender: &MeshSender,
) -> Attempt {
    let pool = state.unicast_pool.clone();
    if pool.send_if_warm(eid, bytes.clone()).await {
        sender.spool(bytes);
        Attempt::Delivered
    } else {
        pool.warm(eid);
        Attempt::FellThrough
    }
}

/// Unicast-only: send if warm, else dial inline (this policy has no gossip
/// fallback, so a cold peer is reached synchronously). A dial failure falls
/// through — [`deliver`]'s central rule turns that into an undeliverable error,
/// not a silent gossip.
async fn try_unicast_only(
    eid: EndpointId,
    bytes: &Bytes,
    state: &EventLoopState,
    sender: &MeshSender,
) -> Attempt {
    let pool = state.unicast_pool.clone();
    if pool.send_if_warm(eid, bytes.clone()).await {
        sender.spool(bytes);
        return Attempt::Delivered;
    }
    if pool.dial_and_send(eid, bytes.clone()).await.is_ok() {
        sender.spool(bytes);
        Attempt::Delivered
    } else {
        Attempt::FellThrough
    }
}

async fn broadcast(sender: &MeshSender, bytes: Bytes) -> Result<()> {
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

    use super::{Lane, Route, lane_for, route};
    use crate::daemon::state::{EventLoopState, MeshSecrets};
    use crate::protocol::identity::Identity;
    use crate::protocol::message::AppFrameParams;
    use crate::protocol::{AppTag, CorrId, MeshId, Message, MessageBody, Nickname};
    use crate::transport::TransportPolicy;

    fn nick(name: &str) -> Nickname {
        Nickname::from(name)
    }

    fn endpoint_id(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn mesh() -> MeshId {
        MeshId::from("💬test")
    }

    fn body() -> MessageBody {
        MessageBody::from("hi")
    }

    /// A meshed state that knows `bob`'s endpoint — the happy path for unicast.
    fn state_knowing_bob() -> (EventLoopState, EndpointId) {
        let mut state = EventLoopState::new(
            None,
            Instant::now(),
            Arc::new(Identity::generate()),
            MeshSecrets::default(),
        );
        state.meshed = true;
        let bob = endpoint_id(1);
        state.participant_endpoints.insert(nick("bob"), bob);
        (state, bob)
    }

    /// A directed frame addressed to `bob` — a `Pong` is the simplest one.
    fn directed_msg() -> Message {
        Message::new_pong(&mesh(), &nick("alice"), nick("bob"))
    }

    const ALL_ON: TransportPolicy = TransportPolicy {
        unicast: true,
        gossip_directed: true,
    };

    // ── the p2p path ──────────────────────────────────────────────────

    #[test]
    fn directed_message_with_known_endpoint_prefers_unicast() {
        let (state, bob) = state_knowing_bob();
        assert_eq!(
            route(&directed_msg(), &state, ALL_ON),
            Route::UnicastPreferred(bob)
        );
    }

    #[test]
    fn directed_rpc_and_pong_also_take_unicast() {
        let (state, bob) = state_knowing_bob();
        let req = Message::new_app(
            &mesh(),
            &nick("alice"),
            AppFrameParams {
                tag: AppTag::from("a2a_req"),
                to: Some(nick("bob")),
                corr: Some(CorrId::from("00000000-0000-0000-0000-0000000000aa")),
                body: body(),
            },
        );
        let pong = Message::new_pong(&mesh(), &nick("alice"), nick("bob"));
        assert_eq!(route(&req, &state, ALL_ON), Route::UnicastPreferred(bob));
        assert_eq!(route(&pong, &state, ALL_ON), Route::UnicastPreferred(bob));
    }

    #[test]
    fn unicast_only_policy_forbids_gossip_fallback() {
        let (state, bob) = state_knowing_bob();
        assert_eq!(
            route(
                &directed_msg(),
                &state,
                TransportPolicy {
                    unicast: true,
                    gossip_directed: false,
                },
            ),
            Route::UnicastOnly(bob)
        );
    }

    /// A known peer we're not yet meshed with is still reached by a unicast
    /// connect — iroh's path selection uses the multihop transport when no direct
    /// path exists, so there is no separate circuit tier to fall to.
    #[test]
    fn known_peer_while_unmeshed_still_attempts_unicast() {
        let (mut state, bob) = state_knowing_bob();
        state.meshed = false;
        assert_eq!(
            route(&directed_msg(), &state, ALL_ON),
            Route::UnicastPreferred(bob)
        );
    }

    // ── the gossip path ───────────────────────────────────────────────

    #[test]
    fn broadcast_message_always_takes_gossip() {
        let (state, _) = state_knowing_bob();
        let open = Message::new_app(
            &mesh(),
            &nick("alice"),
            AppFrameParams {
                tag: AppTag::from("a2a_msg"),
                to: None,
                corr: None,
                body: body(),
            },
        );
        // Even with unicast on and a known peer, a non-directed kind gossips —
        // and stays gossip regardless of policy.
        assert_eq!(route(&open, &state, ALL_ON), Route::Gossip);
        assert_eq!(
            route(
                &open,
                &state,
                TransportPolicy {
                    unicast: false,
                    gossip_directed: true,
                },
            ),
            Route::Gossip
        );
    }

    #[test]
    fn directed_message_with_unknown_endpoint_falls_back_to_gossip() {
        let mut state = EventLoopState::new(
            None,
            Instant::now(),
            Arc::new(Identity::generate()),
            MeshSecrets::default(),
        );
        state.meshed = true; // meshed, but we hold no endpoint for bob
        assert_eq!(route(&directed_msg(), &state, ALL_ON), Route::Gossip);
    }

    #[test]
    fn rendezvous_addressee_is_never_a_directed_target() {
        let mut state = EventLoopState::new(
            None,
            Instant::now(),
            Arc::new(Identity::generate()),
            MeshSecrets::default(),
        );
        state.meshed = true;
        let rendezvous = endpoint_id(9);
        state.rendezvous_id = Some(rendezvous);
        // bob's advertised endpoint *is* the rendezvous pseudo-node.
        state.participant_endpoints.insert(nick("bob"), rendezvous);
        assert_eq!(route(&directed_msg(), &state, ALL_ON), Route::Gossip);
    }

    #[test]
    fn disabling_unicast_routes_directed_to_gossip() {
        let (state, _) = state_knowing_bob();
        // Unicast off + a known peer ⇒ still gossip (the `--no-unicast` switch).
        assert_eq!(
            route(
                &directed_msg(),
                &state,
                TransportPolicy {
                    unicast: false,
                    gossip_directed: true,
                },
            ),
            Route::Gossip
        );
    }

    // ── undeliverable ─────────────────────────────────────────────────

    #[test]
    fn unicast_only_with_unknown_endpoint_is_undeliverable() {
        let mut state = EventLoopState::new(
            None,
            Instant::now(),
            Arc::new(Identity::generate()),
            MeshSecrets::default(),
        );
        state.meshed = true; // no endpoint for bob, and gossip forbidden
        assert_eq!(
            route(
                &directed_msg(),
                &state,
                TransportPolicy {
                    unicast: true,
                    gossip_directed: false,
                },
            ),
            Route::Undeliverable
        );
    }

    #[test]
    fn both_transports_disabled_is_undeliverable_for_directed() {
        let (state, _) = state_knowing_bob();
        assert_eq!(
            route(
                &directed_msg(),
                &state,
                TransportPolicy {
                    unicast: false,
                    gossip_directed: false,
                },
            ),
            Route::Undeliverable
        );
    }

    #[test]
    fn broadcast_survives_both_transports_disabled() {
        // A non-directed kind never hits the undeliverable arm — gossip is
        // structural for broadcasts, not subject to the directed-gossip switch.
        let (state, _) = state_knowing_bob();
        let open = Message::new_app(
            &mesh(),
            &nick("alice"),
            AppFrameParams {
                tag: AppTag::from("a2a_msg"),
                to: None,
                corr: None,
                body: body(),
            },
        );
        assert_eq!(
            route(
                &open,
                &state,
                TransportPolicy {
                    unicast: false,
                    gossip_directed: false,
                },
            ),
            Route::Gossip
        );
    }

    // ── the roster lane ───────────────────────────────────────────────

    #[test]
    fn lane_is_unicast_for_a_meshed_known_peer() {
        let (state, _) = state_knowing_bob();
        assert_eq!(lane_for(&nick("bob"), &state), Lane::Unicast);
    }

    #[test]
    fn lane_is_multihop_for_a_known_but_unmeshed_peer() {
        let (mut state, _) = state_knowing_bob();
        state.meshed = false;
        assert_eq!(lane_for(&nick("bob"), &state), Lane::Multihop);
    }

    #[test]
    fn lane_is_gossip_for_an_unknown_peer() {
        let (state, _) = state_knowing_bob();
        assert_eq!(lane_for(&nick("carol"), &state), Lane::Gossip);
    }
}
