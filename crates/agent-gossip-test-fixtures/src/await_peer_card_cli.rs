use std::time::Instant;

use crate::{MSG_TIMEOUT, POLL, RPC_ATTEMPT, test_cmd};

/// Block until a directed call from `nickname` to `target` completes a round trip.
///
/// This is the subprocess counterpart of
/// [`InProcNode::await_peer_card`](crate::InProcNode::await_peer_card). The probe is a `ListTasks`
/// call, which changes nothing at the peer. It succeeds only when both nodes hold the seal key of
/// the other, from the signed `PeerInfo` of each, because the reply is sealed back.
///
/// # Panics
///
/// Panics if no probe succeeds before `MSG_TIMEOUT`.
pub fn await_peer_card_cli(mesh: &str, nickname: &str, target: &str) {
    let deadline = Instant::now() + MSG_TIMEOUT;
    let timeout = RPC_ATTEMPT.as_secs().to_string();
    loop {
        let probe = test_cmd()
            .args([
                "a2a",
                "call",
                "--gossip",
                mesh,
                "--nickname",
                nickname,
                "--to",
                target,
                "--method",
                "ListTasks",
                "--params",
                "{}",
                "--timeout-secs",
                &timeout,
            ])
            .output()
            .expect("a2a call command failed to spawn");
        if probe.status.success() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no directed round trip from {nickname} to {target} completed"
        );
        std::thread::sleep(POLL);
    }
}
