use crate::InProcNode;

/// Wait until `left` and `right` each hold the *other's* card — the true
/// precondition for a directed A2A **round trip**, and the one a single
/// [`InProcNode::await_peer_card`] does not establish.
///
/// Both legs are sealed, so both directions must have converged. The caller
/// seals the request to the callee's key; the callee then seals its reply back
/// to the *caller's* key — and a responder that lacks the caller's card does not
/// error, it **drops the reply on the floor** (`a2a/node.rs`: "cannot seal a2a
/// rpc response — dropping"). The caller has no way to distinguish that from a
/// slow peer, so it blocks for the full RPC timeout and the test fails ~60s
/// later with "peer unreachable or slow".
///
/// The two directions are not equally likely to be ready, which is why guarding
/// only one hides the bug rather than fixing it: the creator publishes its card
/// when it mints the gossip, so a joiner tends to pick it up during its initial
/// `meta` sync, while the joiner's own card has to propagate *back* afterwards.
/// Waiting on the caller's view alone therefore passes most of the time and
/// flakes under load.
pub async fn await_mutual_cards(left: &InProcNode, right: &InProcNode) {
    left.await_peer_card(&right.nickname).await;
    right.await_peer_card(&left.nickname).await;
}
