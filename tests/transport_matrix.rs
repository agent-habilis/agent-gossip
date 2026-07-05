//! The unified transport matrix: **one** application operation-suite run against
//! **every** transport, proving each transport is compatible with each
//! app-level operation. In-process over the real event loop + a real iroh mesh
//! (`common::InProcNode`), using the per-session `TransportPolicy` so each row
//! forces a different transport without a subprocess.
//!
//! The suite (`run_operations`) drives a broadcast chat, a shared-`state` merge,
//! a `meta` merge, and the full **directed A2A task lifecycle with streaming**
//! (create → worker `working` → artifact → initiator approval → worker
//! `completed`). The rows:
//!
//! - `matrix_default`  — all transports on (unicast-preferred + gossip).
//! - `matrix_unicast`  — `no_gossip_directed`: directed frames take `UnicastOnly`
//!   with **no** fallback, so a completed lifecycle proves unicast.
//! - `matrix_gossip`   — `no_unicast + no_circuit`: directed frames take
//!   `Route::Gossip`, so completion proves gossip.
//! - `matrix_circuit`  — `no_unicast + no_gossip_directed`, with a **synthetic
//!   link-state graph** injected (`wire_circuit`) so a circuit route exists:
//!   directed frames take a circuit with no gossip fallback, so completion
//!   proves circuit. Injected because a live rendezvous-bootstrapped mesh never
//!   converges usable circuit edges — the same reason `src/circuit`'s own tests
//!   build controlled topologies. Gated on the `adversarial` feature.
//! - `matrix_spool`    — default policy + a shared `--spool` dir: the suite runs
//!   as usual and the durable frames are additionally mirrored to disk.
//!
//! Every row is **pure-by-policy** (the forced transport is the only path a
//! directed frame can take), so "the lifecycle completed" is a real proof the
//! named transport carried every directed leg — no per-transport counter needed.

mod common;

use std::time::Instant;

use agent_gossip::{Channel, TaskId, TaskState, TransportPolicy};
use common::{InProcNode, MSG_TIMEOUT, POLL, spool_dir, spool_swarm_dir, wait_for_frames};
use serde_json::{Value, json};

/// Active-view cap for the (2-node) rows that want a full mesh.
const FULL_MESH: usize = 64;

/// `no_gossip_directed`: directed frames take `UnicastOnly`, no gossip fallback.
const UNICAST_ONLY: TransportPolicy = TransportPolicy {
    unicast: true,
    gossip_directed: false,
    circuit: false,
};

/// `no_unicast + no_circuit`: directed frames take `Route::Gossip`.
const GOSSIP_ONLY: TransportPolicy = TransportPolicy {
    unicast: false,
    gossip_directed: true,
    circuit: false,
};

/// `no_unicast + no_gossip_directed`: directed frames take a circuit, with no
/// gossip fallback to mask a circuit failure.
#[cfg(feature = "adversarial")]
const CIRCUIT_ONLY: TransportPolicy = TransportPolicy {
    unicast: false,
    gossip_directed: false,
    circuit: true,
};

/// Parse the `Task` id out of a `SendMessage` response.
fn task_id_of(resp: &Value) -> TaskId {
    resp["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task with an id")
        .parse()
        .expect("the returned id is a valid task id")
}

/// Poll until `node`'s derived `channel` document has `pointer == expected`.
async fn converged(node: &InProcNode, channel: Channel, pointer: &str, expected: &str) -> bool {
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        if node
            .get(channel)
            .await
            .pointer(pointer)
            .and_then(Value::as_str)
            == Some(expected)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The shared operation-suite: every app-level operation between `alice` and
/// `bob`, asserting each completes and converges.
async fn run_operations(alice: &mut InProcNode, bob: &mut InProcNode) {
    // Warmup: a broadcast (always gossip) meshes the peers and exchanges the
    // `PeerInfo`/cards the directed paths resolve on.
    alice.send("warmup").await;
    assert!(
        bob.wait_body("warmup", MSG_TIMEOUT).await,
        "mesh never formed (warmup broadcast not received)"
    );

    // 1. Broadcast chat.
    alice.send("hello over the transport").await;
    assert!(
        bob.wait_body("hello over the transport", MSG_TIMEOUT).await,
        "broadcast chat was not delivered"
    );

    // 2. Shared `state` merge (CRDT convergence).
    alice.state_merge(json!({ "topic": "matrix" })).await;
    assert!(
        converged(bob, Channel::State, "/topic", "matrix").await,
        "shared state never converged on the peer"
    );

    // 3. `meta` merge.
    alice.meta_merge(json!({ "role": "worker" })).await;
    assert!(
        converged(bob, Channel::Meta, "/role", "worker").await,
        "meta never converged on the peer"
    );

    // 4. Directed A2A task lifecycle + streaming — the transport-selectable path.
    let resp = alice.create_task(&bob.nickname, "port the parser").await;
    let task_id = task_id_of(&resp);
    // A multi-frame brief (~20 KB seals past the single-frame cap) proves the
    // sharded RPC plane on this transport: the request splits into shard
    // frames that each ride the forced lane, and the worker still serves it
    // as one logical request.
    let big_brief = format!("big brief {}", "data ".repeat(4 * 1024));
    let big_resp = alice.create_task(&bob.nickname, &big_brief).await;
    assert!(
        big_resp["result"]["task"].is_object(),
        "the multi-frame (sharded) call was not served: {big_resp}"
    );
    assert!(
        bob.wait_task_message(MSG_TIMEOUT).await,
        "the worker never saw the directed task brief"
    );
    bob.task_status(&task_id, TaskState::Working, Some("on it"))
        .await;
    assert!(
        alice.wait_task_state(TaskState::Working, MSG_TIMEOUT).await,
        "the initiator never saw the worker's streamed `working` status"
    );
    bob.task_artifact(&task_id, "here is the port").await;
    assert!(
        alice
            .wait_task_state(TaskState::InputRequired, MSG_TIMEOUT)
            .await,
        "the initiator never saw the streamed artifact / review park"
    );
    alice
        .task_message(&bob.nickname, &task_id, "approved")
        .await;
    bob.task_status(&task_id, TaskState::Completed, Some("done"))
        .await;
    assert!(
        alice
            .wait_task_state(TaskState::Completed, MSG_TIMEOUT)
            .await,
        "the initiator never saw the worker complete the task"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matrix_default() {
    let mut alice = InProcNode::create_with_transport(
        "tm-default",
        "tm-def-a",
        TransportPolicy::DEFAULTS,
        FULL_MESH,
    )
    .await;
    let mut bob = InProcNode::join_with_transport(
        &alice.swarm,
        "tm-def-b",
        TransportPolicy::DEFAULTS,
        FULL_MESH,
    )
    .await;
    run_operations(&mut alice, &mut bob).await;
    alice.leave().await;
    bob.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matrix_unicast() {
    let mut alice =
        InProcNode::create_with_transport("tm-unicast", "tm-uni-a", UNICAST_ONLY, FULL_MESH).await;
    let mut bob =
        InProcNode::join_with_transport(&alice.swarm, "tm-uni-b", UNICAST_ONLY, FULL_MESH).await;
    run_operations(&mut alice, &mut bob).await;
    alice.leave().await;
    bob.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matrix_gossip() {
    let mut alice =
        InProcNode::create_with_transport("tm-gossip", "tm-gos-a", GOSSIP_ONLY, FULL_MESH).await;
    let mut bob =
        InProcNode::join_with_transport(&alice.swarm, "tm-gos-b", GOSSIP_ONLY, FULL_MESH).await;
    run_operations(&mut alice, &mut bob).await;
    alice.leave().await;
    bob.leave().await;
}

/// Circuit: with unicast **and** directed-gossip off, a directed frame has only
/// the circuit transport — and no gossip fallback to mask it — so a completed
/// lifecycle proves the frame rode the circuit (over the circuit ALPN + onion +
/// terminal-delivery path). A live rendezvous-bootstrapped mesh never converges
/// usable circuit edges (participants bridge through the rendezvous rather than
/// becoming direct gossip neighbours), so — exactly as `src/circuit`'s own tests
/// do — we hand each node the routing graph directly with `wire_circuit` (a
/// synthetic 1-hop topology), then drive real app operations over it. Gated on
/// `adversarial`: the injection hook is a testkit escape hatch, not public API.
#[cfg(feature = "adversarial")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matrix_circuit() {
    let mut alice =
        InProcNode::create_with_transport("tm-circuit", "tm-cir-a", CIRCUIT_ONLY, FULL_MESH).await;
    let mut bob =
        InProcNode::join_with_transport(&alice.swarm, "tm-cir-b", CIRCUIT_ONLY, FULL_MESH).await;

    // Stand up the synthetic circuit topology; the graph persists (max-seq) so
    // the daemon's own link-state ticks can't clobber it.
    common::wire_circuit(&alice, &bob).await;

    run_operations(&mut alice, &mut bob).await;

    alice.leave().await;
    bob.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matrix_spool() {
    let dir = spool_dir("matrix");
    let mut alice = InProcNode::create_with_spool("tm-spool", "tm-spl-a", &dir).await;
    let mut bob = InProcNode::join_with_spool(&alice.swarm, "tm-spl-b", &dir).await;

    run_operations(&mut alice, &mut bob).await;

    // The durable operations were additionally mirrored to the shared directory.
    let frame_dir = spool_swarm_dir(&dir, &alice.swarm);
    assert!(
        wait_for_frames(&frame_dir, 1, MSG_TIMEOUT).await >= 1,
        "no .frame files mirrored under {}",
        frame_dir.display()
    );

    alice.leave().await;
    bob.leave().await;
    let _ = std::fs::remove_dir_all(&dir);
}
