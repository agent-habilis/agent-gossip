use std::time::{Duration, Instant};

use crate::{InProcNode, MSG_TIMEOUT};

/// Bring up the links from `from` to every node of `to`.
///
/// The warmup is sent again until all of them have it. A gossip holds no log of a broadcast, so a
/// warmup that leaves before a link exists is lost, and nothing sends it again.
///
/// # Panics
///
/// Panics if a node still lacks the warmup after `MSG_TIMEOUT`.
pub async fn warm_up(from: &InProcNode, to: &mut [&mut InProcNode]) {
    let deadline = Instant::now() + MSG_TIMEOUT;
    for round in 0.. {
        let body = format!("warmup-{round}");
        from.broadcast(&body).await;
        let mut all_received = true;
        for node in to.iter_mut() {
            all_received &= node.wait_body(&body, Duration::from_secs(2)).await;
        }
        if all_received {
            return;
        }
        assert!(Instant::now() < deadline, "nodes never meshed");
    }
}
