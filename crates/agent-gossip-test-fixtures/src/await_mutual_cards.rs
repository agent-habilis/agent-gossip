use crate::InProcNode;

/// Wait until `left` and `right` each hold the entry of the *other* — the true precondition for a
/// directed A2A **round trip**, and the one a single [`InProcNode::await_peer_card`] does not
/// establish.
///
/// Both legs are sealed, so both directions must have converged. The caller seals the request to
/// the key of the callee; the callee then seals its reply to the key of the *caller*. A responder
/// that does not know the key of the caller does not error: it **drops the reply**
/// (`a2a/node.rs`: "cannot seal a2a rpc response — dropping"). The caller cannot tell that from a
/// slow peer, so it blocks for the full RPC timeout. Guarding one direction only hides the bug.
pub async fn await_mutual_cards(left: &InProcNode, right: &InProcNode) {
    left.await_peer_card(&right.nickname).await;
    right.await_peer_card(&left.nickname).await;
}
