//! Filesystem spool tests, in-process over the real event loop, a real iroh
//! mesh, and a **real** `notify` watcher on a real temp directory
//! (`common::InProcNode` + `--spool`). They pin the spool's two promises:
//!
//! - **live mirror / transparency** — with a shared spool dir, normal chat +
//!   shared-state ops still converge, and the outbound tee actually writes
//!   content-addressed `.frame` files;
//! - **sneakernet catch-up** (the strong one) — a node that quits leaves its
//!   frames on disk, and a later node with no lifetime overlap recovers the
//!   shared state from those files alone;
//! - **idempotence / self-echo** — a node re-reading its own spooled frames
//!   never double-surfaces them (pubkey self-drop before dedup).
//!
//! GC and the atomic-write/skip-if-exists/ignore-non-frame rules are unit-tested
//! in `transport::spool` (they need no mesh); this file exercises the wiring.

mod common;

use std::time::Instant;

use common::{InProcNode, MSG_TIMEOUT, POLL, spool_dir, spool_mesh_dir, wait_for_frames};
use serde_json::Value;

/// Live mirror: two meshed peers sharing one spool dir converge as usual, and
/// the shared directory fills with parseable `.frame` files — proof the
/// outbound tee ran without disturbing normal delivery.
#[tokio::test]
async fn live_mirror_writes_frames_and_stays_transparent() {
    let dir = spool_dir("live");
    let creator = InProcNode::create_with_spool("spool-live", "creator", &dir).await;
    let mut joiner = InProcNode::join_with_spool(&creator.mesh, "joiner", &dir).await;

    creator.send("mirrored over the wire").await;
    creator
        .state_merge(serde_json::json!({ "mirror": "on" }))
        .await;

    // Normal delivery is untouched: the joiner still sees the chat and the state.
    assert!(
        joiner.wait_messages(1, MSG_TIMEOUT).await,
        "joiner never received the chat with a shared spool active"
    );
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        if joiner
            .state_get()
            .await
            .pointer("/mirror")
            .and_then(Value::as_str)
            == Some("on")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "joiner never converged on the state"
        );
        tokio::time::sleep(POLL).await;
    }

    // The tee wrote content-addressed frames, and they are real wire JSON.
    let frame_dir = spool_mesh_dir(&dir, &creator.mesh);
    let seen = wait_for_frames(&frame_dir, 1, MSG_TIMEOUT).await;
    assert!(
        seen >= 1,
        "no .frame files were mirrored into {}",
        frame_dir.display()
    );
    let a_frame = std::fs::read_dir(&frame_dir)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "frame"))
        .expect("a committed .frame file");
    let bytes = std::fs::read(a_frame.path()).unwrap();
    serde_json::from_slice::<Value>(&bytes).expect("a spooled frame is wire JSON");

    creator.leave().await;
    joiner.leave().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Sneakernet catch-up: the creator writes state to the spool, then quits. A
/// joiner with the **same** spool dir and no lifetime overlap recovers the
/// shared state from the on-disk frames alone (channel ingest is horizon-
/// independent). We deliberately do NOT assert the joiner *surfaces* the
/// creator's pre-join chat — the join horizon gates that by design; only the
/// CRDT state doc is expected to reflect it.
#[tokio::test]
async fn sneakernet_catch_up_recovers_state_from_files() {
    let dir = spool_dir("net");
    let mesh = {
        let creator = InProcNode::create_with_spool("spool-net", "sender", &dir).await;
        creator
            .state_merge(serde_json::json!({ "topic": "sneakernet" }))
            .await;
        creator.send("carried across, not connected").await;

        // Ensure the frames are durably on disk before the sender exits — the
        // writer task mirrors asynchronously, so a race here would let the
        // joiner start against an empty directory.
        let frame_dir = spool_mesh_dir(&dir, &creator.mesh);
        let seen = wait_for_frames(&frame_dir, 1, MSG_TIMEOUT).await;
        assert!(seen >= 1, "sender wrote no frames to spool before leaving");

        let mesh = creator.mesh.clone();
        creator.leave().await; // clean shutdown; the .frame files persist
        mesh
    };

    // No overlap: the sender is gone. Only the files can carry the state.
    let joiner = InProcNode::join_with_spool(&mesh, "receiver", &dir).await;
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let doc = joiner.state_get().await;
        if doc.pointer("/topic").and_then(Value::as_str) == Some("sneakernet") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "joiner never ingested the sender's spooled state; doc = {doc}"
        );
        tokio::time::sleep(POLL).await;
    }

    joiner.leave().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Idempotence / self-echo: a node's own watcher re-reads the frames it just
/// wrote, but `gossip::ingest` drops them on the pubkey self-echo check (before
/// dedup), so its own messages surface exactly once — never doubled by the
/// spool round-trip.
#[tokio::test]
async fn own_spooled_frames_are_not_resurfaced() {
    let dir = spool_dir("echo");
    let mut node = InProcNode::create_with_spool("spool-echo", "solo", &dir).await;

    for text in ["one", "two", "three"] {
        node.send(text).await;
    }

    // Wait until the watcher has had the frames to re-read (≥3 committed), then
    // give it a beat to re-ingest so a broken self-drop would double-count.
    let frame_dir = spool_mesh_dir(&dir, &node.mesh);
    let seen = wait_for_frames(&frame_dir, 3, MSG_TIMEOUT).await;
    assert!(
        seen >= 3,
        "expected the three chat frames on disk, saw {seen}"
    );
    tokio::time::sleep(POLL * 4).await;

    let msgs = node.msg_events();
    assert_eq!(
        msgs.len(),
        3,
        "own messages surfaced {} times — spool re-ingest was not self-dropped",
        msgs.len()
    );

    node.leave().await;
    let _ = std::fs::remove_dir_all(&dir);
}
