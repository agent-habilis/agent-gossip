use crate::{InProcNode, warm_up};

/// A creator `<name>-alice` and a joiner `bob_nick`, ready for a directed msg from the creator.
///
/// The warmup proves that the mesh formed, and the peer card is what a
/// directed msg is sealed to: without both, the first send can fail.
pub async fn meshed_pair(name: &str, bob_nick: &str) -> (InProcNode, InProcNode) {
    let alice = InProcNode::create_with_nick(name, &format!("{name}-alice")).await;
    let mut bob = InProcNode::join(&alice.mesh, bob_nick).await;
    warm_up(&alice, &mut [&mut bob]).await;
    alice.await_peer_card(bob_nick).await;
    (alice, bob)
}
