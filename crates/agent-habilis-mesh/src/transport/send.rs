//! The single send decision. Every outbound message funnels through
//! [`deliver`]: a broadcast (no sole addressee) rides gossip, structurally; a
//! directed message goes over the unicast connection only — warm pooled
//! connection first, inline dial otherwise — and gossip never carries it. The
//! bytes are the same canonical wire `Message` on both planes, so the
//! receiver's path is identical either way.

use anyhow::{Context, Result};
use bytes::Bytes;
use iroh::EndpointId;

use crate::daemon::state::EventLoopState;
use crate::protocol::message::sole_addressee;
use crate::protocol::{Message, Nickname};
use crate::transport::MeshSender;

/// Route one already-signed, already-serialized message. `bytes` MUST be
/// `msg.serialize()` — the same wire form gossip uses.
///
/// # Errors
/// Propagates a gossip broadcast failure or a unicast dial/send failure. A
/// directed message whose addressee has no dialable endpoint (its `PeerInfo`
/// hasn't arrived, or its advertised endpoint is the rendezvous pseudo-node)
/// is undeliverable — the error names which; callers retry once a real
/// endpoint is learned.
pub async fn deliver(
    msg: &Message,
    bytes: Bytes,
    state: &EventLoopState,
    sender: &MeshSender,
) -> Result<()> {
    match route(msg, state) {
        Route::Broadcast => broadcast(sender, bytes).await,
        Route::Unicast(eid) => send_unicast(eid, bytes, state).await,
        Route::Undeliverable => {
            let addressee = sole_addressee(&msg.kind);
            let name = addressee.map_or("<none>", Nickname::as_str);
            // Two distinct causes hide behind one route: waiting on PeerInfo
            // is retryable, an addressee advertising the rendezvous is not.
            let via_rendezvous = addressee
                .and_then(|nick| state.participant_endpoints.get(nick))
                .is_some_and(|eid| state.rendezvous_id == Some(*eid));
            let cause = if via_rendezvous {
                "its advertised endpoint is the rendezvous pseudo-node, never a directed target"
            } else {
                "no known endpoint (PeerInfo not yet received)"
            };
            Err(anyhow::anyhow!(
                "directed message to {name} undeliverable: {cause}"
            ))
        }
    }
}

/// The chosen transport for a message — a **pure** decision so both planes are
/// unit-testable without touching the network.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// No sole addressee: gossip is the transport, structurally.
    Broadcast,
    /// Directed to a known endpoint: unicast only. The underlying iroh
    /// connection uses a direct path when one exists, else the registered
    /// multihop transport — the send decision doesn't distinguish.
    Unicast(EndpointId),
    /// Directed with no known endpoint: an error, never gossip.
    Undeliverable,
}

fn route(msg: &Message, state: &EventLoopState) -> Route {
    let Some(nick) = sole_addressee(&msg.kind) else {
        return Route::Broadcast;
    };
    match directed_endpoint(nick, state) {
        Some(eid) => Route::Unicast(eid),
        None => Route::Undeliverable,
    }
}

/// The lane a directed frame to `nick` would take right now — [`Route`]
/// stripped of its endpoint payload so the roster can serialize it. Sharing
/// [`directed_endpoint`] keeps the surfaced column from ever drifting from the
/// real send decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    Unicast,
    Multihop,
    Unreachable,
}

/// The roster's `transport` column. A directed frame to a known peer takes a
/// unicast connect; we label it `unicast` when a *direct* path is expected (the
/// peer is in our gossip mesh) and `multihop` otherwise — a best-effort hint,
/// since the actual path is chosen by iroh at connect time.
pub(crate) fn lane_for(nick: &Nickname, state: &EventLoopState) -> Lane {
    if directed_endpoint(nick, state).is_none() {
        return Lane::Unreachable;
    }
    if directly_meshed(nick, state) {
        Lane::Unicast
    } else {
        Lane::Multihop
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

/// Warm pooled connection first (steady state, no dial — an optimistic
/// fire-and-forget handoff; a write that fails on a dead connection redials
/// and resends once in the background); a cold peer is dialed inline, and the
/// dial error is the send's outcome — there is no fallback.
async fn send_unicast(eid: EndpointId, bytes: Bytes, state: &EventLoopState) -> Result<()> {
    let pool = state.unicast_pool.clone();
    if pool.send_if_warm(eid, bytes.clone()).await {
        return Ok(());
    }
    pool.dial_and_send(eid, bytes)
        .await
        .with_context(|| format!("unicast dial to {eid} failed"))
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

    fn empty_state() -> EventLoopState {
        EventLoopState::new(
            None,
            Instant::now(),
            Arc::new(Identity::generate()),
            MeshSecrets::default(),
        )
    }

    /// A meshed state that knows `bob`'s endpoint — the happy path for unicast.
    fn state_knowing_bob() -> (EventLoopState, EndpointId) {
        let mut state = empty_state();
        state.meshed = true;
        let bob = endpoint_id(1);
        state.participant_endpoints.insert(nick("bob"), bob);
        (state, bob)
    }

    /// A directed frame addressed to `bob` — a `Pong` is the simplest one.
    fn directed_msg() -> Message {
        Message::new_pong(&mesh(), &nick("alice"), nick("bob"))
    }

    fn open_broadcast() -> Message {
        Message::new_app(
            &mesh(),
            &nick("alice"),
            AppFrameParams {
                tag: AppTag::from("a2a_msg"),
                to: None,
                corr: None,
                body: body(),
            },
        )
    }

    // ── the p2p path ──────────────────────────────────────────────────

    #[test]
    fn directed_message_with_known_endpoint_takes_unicast() {
        let (state, bob) = state_knowing_bob();
        assert_eq!(route(&directed_msg(), &state), Route::Unicast(bob));
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
        assert_eq!(route(&req, &state), Route::Unicast(bob));
        assert_eq!(route(&pong, &state), Route::Unicast(bob));
    }

    /// A known peer we're not yet meshed with is still reached by a unicast
    /// connect — iroh's path selection uses the multihop transport when no direct
    /// path exists, so mesh membership doesn't gate the directed send.
    #[test]
    fn known_peer_while_unmeshed_still_takes_unicast() {
        let (mut state, bob) = state_knowing_bob();
        state.meshed = false;
        assert_eq!(route(&directed_msg(), &state), Route::Unicast(bob));
    }

    // ── the gossip plane (broadcasts only) ────────────────────────────

    #[test]
    fn broadcast_message_always_takes_gossip() {
        let (state, _) = state_knowing_bob();
        // Even with a known peer, a non-directed kind is a broadcast.
        assert_eq!(route(&open_broadcast(), &state), Route::Broadcast);
    }

    // ── undeliverable ─────────────────────────────────────────────────

    #[test]
    fn directed_message_with_unknown_endpoint_is_undeliverable() {
        let mut state = empty_state();
        state.meshed = true; // meshed, but we hold no endpoint for bob
        assert_eq!(route(&directed_msg(), &state), Route::Undeliverable);
    }

    #[test]
    fn rendezvous_addressee_is_never_a_directed_target() {
        let mut state = empty_state();
        state.meshed = true;
        let rendezvous = endpoint_id(9);
        state.rendezvous_id = Some(rendezvous);
        // bob's advertised endpoint *is* the rendezvous pseudo-node.
        state.participant_endpoints.insert(nick("bob"), rendezvous);
        assert_eq!(route(&directed_msg(), &state), Route::Undeliverable);
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
    fn lane_is_unreachable_for_an_unknown_peer() {
        let (state, _) = state_knowing_bob();
        assert_eq!(lane_for(&nick("carol"), &state), Lane::Unreachable);
    }
}
