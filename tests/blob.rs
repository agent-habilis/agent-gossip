//! Blob-channel integration (in-process, via the `InProcNode` harness): a
//! worker returns a **file** as its task result. The file is offloaded over the
//! blob channel and the emitted `TaskArtifactUpdate` carries a `Part.url`
//! reference (a `🎟️…` ticket) rather than inline bytes — and the initiator
//! observes the artifact (review park). The point-to-point fetch + SHA-256
//! verification is unit-tested in `src/blob` (loopback round-trip + adversarial).

mod common;

use std::time::Duration;

use agent_mesh::TaskState;
use common::{InProcNode, MSG_TIMEOUT};

const TASK_WAIT: Duration = MSG_TIMEOUT;

/// A worker result that is a file becomes a `Part.url` blob reference on the
/// artifact — never inline bytes — and the initiator sees the review park.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_result_is_offloaded_as_a_url_reference() {
    let mut alice = InProcNode::create("t-blob").await;
    let mut bob = InProcNode::join(&alice.mesh, "t-blob-bob").await;
    alice.send("warmup").await;
    assert!(bob.wait_body("warmup", MSG_TIMEOUT).await, "mesh formed");
    // Prove the reverse path too: the RPC response rides bob's outbound, and
    // A2aReq/A2aResp are not loggable (no anti-entropy healing), so a reply
    // broadcast into a still-converging overlay is lost for good and the call
    // dies as a 60s waiter timeout — the suite's long-standing flake.
    bob.send("warmup-back").await;
    assert!(alice.wait_body("warmup-back", MSG_TIMEOUT).await, "reverse");

    // `create_task` waits for bob's card (the directed-seal key) before sending.
    let resp = alice.create_task("t-blob-bob", "render the report").await;
    let task_id = resp["result"]["task"]["id"]
        .as_str()
        .expect("worker returned a Task id")
        .parse()
        .expect("valid task id");
    assert!(bob.wait_task_message(TASK_WAIT).await, "bob saw the brief");

    // A result file the worker "produced".
    let path =
        std::env::temp_dir().join(format!("agent-mesh-blob-it-{}.bin", std::process::id()));
    std::fs::write(&path, vec![0x5au8; 128 * 1024]).expect("write result file");

    bob.task_artifact_file(&task_id, &path).await;

    // The initiator (addressee) decrypts the sealed artifact and surfaces it
    // (parks in input-required for review). The artifact body is sealed on the
    // wire, so we read it from the initiator's *decrypted* task event, not the
    // worker's own sealed echo.
    assert!(
        alice
            .wait_task_state(TaskState::InputRequired, TASK_WAIT)
            .await,
        "initiator never saw the file result"
    );
    let (artifact, _) = alice
        .tasks()
        .into_iter()
        .find(|(msg, _)| {
            agent_mesh::a2a::gossip::frame_task_state(msg) == Some(TaskState::InputRequired)
        })
        .expect("initiator surfaced the artifact");
    let payload: serde_json::Value =
        serde_json::from_str(artifact.body.as_str()).expect("decrypted artifact payload is JSON");
    let part = &payload["artifact"]["parts"][0];
    let url = part["url"].as_str().expect("the file part carries a url");
    assert!(
        url.starts_with("🎟️"),
        "the result must be a 🎟️ blob reference, got: {url}"
    );
    assert!(
        part["raw"].is_null(),
        "the bytes must NOT be inlined, got raw: {}",
        part["raw"]
    );

    std::fs::remove_file(&path).ok();
    alice.leave().await;
    bob.leave().await;
}
