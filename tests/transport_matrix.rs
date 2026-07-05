//! Transport-matrix tests: the same user-visible operations — broadcast chat,
//! directed A2A RPC (request + response), worker status/artifact pushes, task
//! follow-ups — delivered with the session restricted to a single transport
//! lane (unicast-only, gossip-only, circuit-only). This pins the transparency
//! invariant: the transport is a routing decision, never a semantic one, so
//! every operation must survive on any lane.
//!
//! A restricted lane may briefly have no route (endpoints arrive via
//! `PeerInfo`, circuits need link-state convergence on its 15s
//! cadence); a directed call then surfaces as an RPC timeout rather than an
//! error, so the harness retries short-timeout calls until the lane
//! converges.

mod common;

use std::time::{Duration, Instant};

use agent_gossip::{Nickname, TaskId, TaskState, TransportPolicy};
use common::{InProcNode, MSG_TIMEOUT, POLL, RECOVERY_TIMEOUT};

const UNICAST_ONLY: TransportPolicy = TransportPolicy {
    unicast: true,
    gossip_directed: false,
    circuit: false,
};

const GOSSIP_ONLY: TransportPolicy = TransportPolicy {
    unicast: false,
    gossip_directed: true,
    circuit: false,
};

const CIRCUIT_ONLY: TransportPolicy = TransportPolicy {
    unicast: false,
    gossip_directed: false,
    circuit: true,
};

/// Parse the `Task` id out of a `SendMessage` response.
fn task_id_of(resp: &serde_json::Value) -> TaskId {
    resp["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task with an id")
        .parse()
        .expect("the returned id is a valid task id")
}

/// Directed `SendMessage` retried with a short per-attempt timeout until the
/// lane's route converges or the recovery budget elapses. Retrying re-sends
/// the same A2A `messageId`, so a served-but-unanswered attempt doesn't
/// change what the assertions observe.
async fn create_task_when_routable(
    from: &InProcNode,
    to: &str,
    text: &str,
) -> serde_json::Value {
    from.await_peer_card(to).await;
    let msg = agent_gossip::a2a::gossip::send_message_payload(from.session.swarm_id(), None, text);
    let params = serde_json::json!({ "message": msg });
    let deadline = Instant::now() + RECOVERY_TIMEOUT;
    loop {
        let resp = from
            .session
            .a2a_call(
                Nickname::new(to).expect("valid target"),
                "SendMessage".to_owned(),
                params.clone(),
                Duration::from_secs(10),
            )
            .await
            .expect("a2a_call transport");
        if resp["result"].is_object() {
            return resp;
        }
        assert!(
            Instant::now() < deadline,
            "directed call never converged on this lane: {resp}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The full operation set over one lane: broadcast both ways, directed task
/// creation (RPC round-trip), worker status + artifact pushes, an initiator
/// follow-up into the live task, and the worker's terminal `completed`.
async fn lane_carries_all_operations(tag: &str, transport: TransportPolicy) {
    let mut initiator =
        InProcNode::create_with_transport(&format!("lane-{tag}"), &format!("init-{tag}"), transport)
            .await;
    let mut worker =
        InProcNode::join_with_transport(&initiator.swarm, &format!("work-{tag}"), transport).await;

    // Broadcast chat is gossip-structural on every lane (and doubles as the
    // mesh warmup) — assert it both directions.
    initiator.send(&format!("hello-{tag}")).await;
    assert!(
        worker.wait_body(&format!("hello-{tag}"), MSG_TIMEOUT).await,
        "[{tag}] broadcast msg initiator→worker never arrived"
    );
    worker.send(&format!("echo-{tag}")).await;
    assert!(
        initiator
            .wait_body(&format!("echo-{tag}"), MSG_TIMEOUT)
            .await,
        "[{tag}] broadcast msg worker→initiator never arrived"
    );

    // Directed RPC round-trip: A2aReq out, the worker serves, A2aResp back.
    let resp =
        create_task_when_routable(&initiator, &worker.nickname.clone(), &format!("brief-{tag}"))
            .await;
    let task_id = task_id_of(&resp);
    assert!(
        worker.wait_task_message(MSG_TIMEOUT).await,
        "[{tag}] the worker never surfaced the incoming task message"
    );

    // Worker pushes ride the directed plane: A2aStatus, then A2aArtifact
    // (which parks the task in input-required).
    worker
        .task_status(&task_id, TaskState::Working, Some("on it"))
        .await;
    assert!(
        initiator
            .wait_task_state(TaskState::Working, MSG_TIMEOUT)
            .await,
        "[{tag}] initiator never saw the worker accept"
    );
    worker
        .task_artifact(&task_id, &format!("result-{tag}"))
        .await;
    assert!(
        initiator
            .wait_task_state(TaskState::InputRequired, MSG_TIMEOUT)
            .await,
        "[{tag}] initiator never saw the artifact / review park"
    );

    // A large brief exercises the sharded RPC plane on this lane: the sealed
    // request splits into shard-tagged frames that each ride the lane
    // individually, and the worker still serves it as one logical request.
    let big_brief = format!("brief-{tag}-big {}", "data ".repeat(4 * 1024)); // ~20 KB
    let big_resp =
        create_task_when_routable(&initiator, &worker.nickname.clone(), &big_brief).await;
    assert!(
        big_resp["result"]["task"].is_object(),
        "[{tag}] the multi-frame call was not served: {big_resp}"
    );

    // Initiator follow-up into the live task (another RPC round-trip), then
    // the worker authors the terminal state.
    let approve = initiator
        .task_message(&worker.nickname.clone(), &task_id, "approved")
        .await;
    assert!(
        approve["result"]["task"].is_object(),
        "[{tag}] approval not acked: {approve}"
    );
    worker
        .task_status(&task_id, TaskState::Completed, Some("done"))
        .await;
    assert!(
        initiator
            .wait_task_state(TaskState::Completed, MSG_TIMEOUT)
            .await,
        "[{tag}] initiator never saw the worker complete the task"
    );

    initiator.leave().await;
    worker.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unicast_only_lane_carries_all_operations() {
    lane_carries_all_operations("uni", UNICAST_ONLY).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gossip_only_lane_carries_all_operations() {
    lane_carries_all_operations("gos", GOSSIP_ONLY).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn circuit_only_lane_carries_all_operations() {
    lane_carries_all_operations("cir", CIRCUIT_ONLY).await;
}
