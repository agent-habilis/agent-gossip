use crate::InProcNode;

/// In-process analogue of the subprocess `three_peers`: a creator
/// plus two joiners (`mon-<suffix>-a` / `-b`), all meshed in this
/// process. The mesh id is `creator.mesh`.
pub async fn three_peers(suffix: &str) -> (InProcNode, InProcNode, InProcNode) {
    let creator = InProcNode::create(&format!("mon{suffix}")).await;
    let joiner_a = InProcNode::join(&creator.mesh, &format!("mon-{suffix}-a")).await;
    let joiner_b = InProcNode::join(&creator.mesh, &format!("mon-{suffix}-b")).await;
    (creator, joiner_a, joiner_b)
}
