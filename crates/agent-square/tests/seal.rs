//! End-to-end sealing (in-process, via the `InProcNode` harness): a directed
//! A2A frame is encrypted to its recipient, so a third peer (a relay) forwards
//! it but never surfaces its content, while the addressee decrypts it through
//! the real event loop. Broadcast stays public — every member reads it. The
//! sealed-box crypto itself (round-trip, wrong key, tamper) is unit-tested in
//! `src/protocol/seal.rs`.

mod common;

use std::time::Duration;

use common::{InProcNode, MSG_TIMEOUT};

/// A directed task A→B is readable only by B; the relay C never surfaces it.
/// Broadcast from A reaches both B and C.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directed_frames_are_private_to_the_recipient_broadcast_stays_public() {
    let alice = InProcNode::create("seal").await;
    let mut bob = InProcNode::join(&alice.mesh, "seal-bob").await;
    let mut carol = InProcNode::join(&alice.mesh, "seal-carol").await;

    // Warm the mesh; a broadcast reaches everyone (proves C is a live relay).
    alice.send("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "bob meshed");
    assert!(carol.wait_body("warmup", MSG_TIMEOUT).await, "carol meshed");

    // A delegates a task to B — a directed, sealed `message/send`.
    let resp = alice
        .create_task("seal-bob", "the private brief for bob")
        .await;
    assert!(resp["result"]["task"].is_object(), "task created: {resp}");

    // B is the addressee: it decrypts the sealed brief and surfaces it.
    assert!(
        bob.wait_task_message(MSG_TIMEOUT).await,
        "bob must decrypt and surface the directed task brief"
    );
    // C relays the same frame but cannot read the sealed body, so it never
    // surfaces the A→B task.
    assert!(
        !carol.wait_task_message(Duration::from_secs(2)).await,
        "carol (a relay) must not surface a task it is not addressed by"
    );

    // Broadcast stays public: A's chat is readable by every member, C included.
    alice.send("a public announcement").await;
    assert!(
        bob.wait_body("a public announcement", MSG_TIMEOUT).await,
        "bob reads broadcast"
    );
    assert!(
        carol.wait_body("a public announcement", MSG_TIMEOUT).await,
        "carol reads broadcast — broadcast is not sealed"
    );

    alice.leave().await;
    bob.leave().await;
    carol.leave().await;
}
