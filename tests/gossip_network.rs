//! Integration tests for the gossip network.
//!
//! Each test spawns real `ahsw` processes, exercises the network,
//! and asserts on what each node actually received. Tests are independent —
//! each creates its own swarm so IPC sockets never collide.
//!
//! Run `cargo build --release` first for faster crypto (shorter connect times).
mod common;

use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{
    CONNECT_TIMEOUT, InProcNode, MSG_TIMEOUT, Msg, Node, POLL, RECOVERY_TIMEOUT, RUNTIME_DIR, bin,
    cli_message, cli_message_raw, cli_msg_checked, cli_msg_stdout, cli_notice, cli_peers, cli_ping,
    cli_poll, cli_poll_long, ipc_raw, serial_guard, socket_path, tmp_log, trace_log, wait_total,
    wait_until,
};
use serde_json::json;

/// How long a survivor needs to claim a dead beacon's seed-derived
/// rendezvous (the claim-if-free handoff). The heal tick is a fixed 15s
/// `const`, and a claim takes **≥2 cycles** (the first confirms the old
/// beacon is gone, the next binds) — so the real floor is ~34s. We wait
/// `2 * HEAL_INTERVAL_SECS + margin` before a fresh peer can bootstrap
/// through the migrated beacon. Irreducible (the cadence is a `const`),
/// and shared by every post-departure-join test so they can't drift to a
/// too-short value (the cause of a flaky `test_first_message_…`).
const RENDEZVOUS_HANDOFF: Duration = Duration::from_secs(36);

// ── tests ─────────────────────────────────────────────────────────────────────

/// Wire contract for the `info` IPC + `doctor` active-swarms scan: a live
/// daemon answers `info` over its socket with its own identity, so
/// `doctor --output json` lists it under "Active swarms" with the full swarm
/// id and name. `--no-probe` keeps this to the fast local-socket scan (no
/// net-report), so the test stays offline-safe.
#[test]
fn doctor_lists_active_swarm_as_json() {
    let _serial = serial_guard();

    let (node, swarm) = Node::create_named("itest-doctor");
    assert!(node.wait_ready(&swarm), "daemon never ready");

    let out = common::test_cmd()
        .args(["doctor", "--no-probe", "--output", "json"])
        .output()
        .expect("doctor command failed to spawn");
    assert!(
        out.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"sections\""),
        "doctor json shape:\n{stdout}"
    );
    assert!(
        stdout.contains("Active swarms"),
        "no Active swarms section:\n{stdout}"
    );
    assert!(
        stdout.contains(swarm.as_str()),
        "active swarm {swarm} not listed:\n{stdout}"
    );
    assert!(
        stdout.contains("itest-doctor"),
        "swarm name not listed:\n{stdout}"
    );
}

/// Basic sanity: a broadcast message is received by the non-sending node.
/// The node whose IPC socket is used will NOT receive its own broadcast;
/// the peer will. We check total delivery across both nodes = 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_two_node_message_delivery() {
    let creator = InProcNode::create("net2node").await;
    let mut joiner = InProcNode::join(&creator.swarm, "joiner-2node").await;

    creator.send("hello from the network").await;

    assert!(
        joiner.wait_inbound(1, MSG_TIMEOUT).await,
        "joiner never received the creator's message"
    );
    let received = joiner.inbound();
    assert_eq!(received.len(), 1, "expected exactly 1 delivery");
    assert_eq!(received[0].body.as_str(), "hello from the network");
}

/// A passworded swarm: the right password joins and messages flow; a wrong
/// password fails locally against the id's verifier ("wrong password"), and
/// no password at all is a typed "password-protected" error — both before
/// any network traffic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_passworded_swarm_verifies_locally_and_meshes() {
    let creator = InProcNode::create_with_password("netpw", "hunter2").await;

    // Wrong password: rejected by the verifier carried in the id.
    let Err(wrong) =
        InProcNode::try_join_with_password(&creator.swarm, "joiner-bad", "hunter3").await
    else {
        panic!("a wrong password must be rejected");
    };
    assert!(wrong.to_string().contains("wrong password"), "got: {wrong}");

    // No password: a crisp requirement error, not a silent empty swarm.
    let target = creator.swarm.parse().expect("join target");
    let missing = agent_habilis_swarm::embed::SwarmSession::join(
        agent_habilis_swarm::embed::JoinConfig::new(target),
    )
    .await
    .expect_err("a missing password must be rejected");
    assert!(
        missing.to_string().contains("password-protected"),
        "got: {missing}"
    );

    // The right password lands on the same topic and messages flow.
    let mut joiner = InProcNode::try_join_with_password(&creator.swarm, "joiner-good", "hunter2")
        .await
        .expect("the right password joins");
    creator.send("hello behind the password").await;
    assert!(
        joiner.wait_inbound(1, MSG_TIMEOUT).await,
        "passworded joiner never received the creator's message"
    );
    assert_eq!(
        joiner.inbound()[0].body.as_str(),
        "hello behind the password"
    );
}

/// The subprocess wire contract for `--password`: `create --password=pw`
/// mints a passworded id; `join` without the password exits non-zero with
/// "password-protected" under `--no-interactive`, a wrong password exits
/// non-zero with "wrong password", and `join --password=pw` meshes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_password_flag_wire_contract() {
    let (creator, swarm_id) = Node::create_args("pwwire", &["--password=hunter2"], &[]);
    assert!(creator.wait_ready(&swarm_id), "creator never became ready");

    // Missing password, non-interactive: crisp requirement error.
    let missing = common::test_cmd()
        .args(["join", &swarm_id, "--no-interactive"])
        .output()
        .expect("spawn join without password");
    assert!(
        !missing.status.success(),
        "missing password must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("password-protected"),
        "stderr must name the requirement, got: {stderr}"
    );

    // Wrong password: rejected locally against the id's verifier.
    let wrong = common::test_cmd()
        .args(["join", &swarm_id, "--no-interactive", "--password=hunter3"])
        .output()
        .expect("spawn join with wrong password");
    assert!(!wrong.status.success(), "wrong password must exit non-zero");
    let wrong_stderr = String::from_utf8_lossy(&wrong.stderr);
    assert!(
        wrong_stderr.contains("wrong password"),
        "stderr must say wrong password, got: {wrong_stderr}"
    );

    // The right password joins and meshes (the join's ready socket appears).
    let joiner = Node::join_args(&swarm_id, "pw-joiner", &["--password=hunter2"], &[]);
    assert!(
        joiner.wait_ready(&swarm_id),
        "passworded joiner never became ready: {}",
        joiner.log_tail(20)
    );
    joiner.kill();
    creator.kill();
}

/// Durable state log, live propagation: a creator patches the shared state; a
/// meshed joiner converges to the same derived document via gossip. State rides
/// the same topic but its own un-pruned log, surfaced through `state get`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_state_log_propagates_to_a_peer() {
    let creator = InProcNode::create("netstate").await;
    let joiner = InProcNode::join(&creator.swarm, "joiner-state").await;

    // Mesh first (a delivered message proves the link), so the state events
    // broadcast onto a live overlay rather than the unmeshed buffer.
    creator.send("link").await;

    creator.state_merge(json!({"alpha": 1})).await;
    creator.state_merge(json!({"beta": 2})).await;

    let want = json!({"alpha": 1, "beta": 2});
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut got = joiner.state_get().await;
    while got != want && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        got = joiner.state_get().await;
    }
    assert_eq!(
        got, want,
        "joiner never converged to the creator's state log"
    );
    // The author holds its own events too (gossip never echoes to self).
    assert_eq!(creator.state_get().await, want);
}

/// Durable state log, anti-entropy backfill: a peer that joins **after** the
/// state events are live (so nothing is buffered to flush to it) must still
/// reconstruct the full un-pruned log via the state digest — the durability
/// guarantee. Three nodes: creator+early are meshed so the appends broadcast
/// live (not into the unmeshed buffer); the late joiner can only pull.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_state_log_backfills_a_late_joiner() {
    let creator = InProcNode::create("netstatelate").await;
    let early = InProcNode::join(&creator.swarm, "early-state").await;
    // Mesh so appends go out live, leaving the creator's outbound buffer empty.
    creator.send("link").await;

    creator.state_merge(json!({"alpha": 1})).await;
    creator.state_merge(json!({"beta": 2})).await;

    let want = json!({"alpha": 1, "beta": 2});
    // Confirm the live path first.
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut early_got = early.state_get().await;
    while early_got != want && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        early_got = early.state_get().await;
    }
    assert_eq!(early_got, want, "early peer never got the live state");

    // The late joiner arrives after all state traffic; only anti-entropy can
    // backfill it (within an antientropy interval once it advertises its set).
    let late = InProcNode::join(&creator.swarm, "late-state").await;
    let late_deadline = Instant::now() + RECOVERY_TIMEOUT;
    let mut late_got = late.state_get().await;
    while late_got != want && Instant::now() < late_deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        late_got = late.state_get().await;
    }
    assert_eq!(
        late_got, want,
        "late joiner never backfilled state via anti-entropy"
    );
}

/// Three-node full-mesh: a broadcast should reach at least 2 of the 3 nodes
/// (the sender never receives its own broadcast). This test also documents
/// the known HyParView relay bug where the second joiner may not receive messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_three_node_full_delivery() {
    let alpha = InProcNode::create("net3node").await;
    let mut beta = InProcNode::join(&alpha.swarm, "beta-3node").await;
    let mut gamma = InProcNode::join(&alpha.swarm, "gamma-3node").await;

    alpha.send("broadcast to all three nodes").await;

    // The sender never receives its own broadcast; the other two
    // should (>=2 deliveries across beta+gamma).
    assert!(
        beta.wait_inbound(1, MSG_TIMEOUT).await && gamma.wait_inbound(1, MSG_TIMEOUT).await,
        "expected both beta and gamma to receive the broadcast"
    );
    for msg in beta.inbound().into_iter().chain(gamma.inbound()) {
        assert_eq!(msg.body.as_str(), "broadcast to all three nodes");
    }
}

/// A reply addressed to its target is delivered to the addressee.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_reply_delivery() {
    let mut creator = InProcNode::create("netreply").await;
    let mut joiner = InProcNode::join(&creator.swarm, "joiner-reply").await;

    creator.send("what is 2 + 2?").await;
    assert!(
        joiner.wait_inbound(1, MSG_TIMEOUT).await,
        "joiner never got the question"
    );

    joiner.reply(&creator.nickname, "4").await;
    assert!(
        creator.wait_body("4", MSG_TIMEOUT).await,
        "creator (the addressee) never received the reply"
    );
}

/// Directed replies are only surfaced to the addressee, not to uninvolved
/// peers. In a 3-node swarm, if A sends and B replies to A, observer C
/// should NOT see the reply — saving tokens for peers it is not addressed to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_reply_only_visible_to_addressee() {
    let mut alpha = InProcNode::create("netfilter").await;
    let mut beta = InProcNode::join(&alpha.swarm, "beta-filter").await;
    let mut gamma = InProcNode::join(&alpha.swarm, "gamma-filter").await;

    alpha.send("filter-test message").await;
    assert!(
        beta.wait_inbound(1, MSG_TIMEOUT).await && gamma.wait_inbound(1, MSG_TIMEOUT).await,
        "alpha's message never reached beta and gamma"
    );

    beta.reply(&alpha.nickname, "filter-test reply").await;

    // Alpha (the addressee) MUST see the reply.
    assert!(
        alpha.wait_body("filter-test reply", MSG_TIMEOUT).await,
        "alpha (the addressee) should see the directed reply but didn't"
    );

    // Gamma (uninvolved) must NOT see it — directed replies are
    // filtered at the receiver for non-addressees.
    assert!(
        !gamma
            .inbound()
            .iter()
            .any(|msg| msg.body.as_str() == "filter-test reply"),
        "gamma (uninvolved) should NOT see the directed reply but did"
    );
}

/// With `--no-gossip-directed`, gossip carries no directed traffic, so a
/// directed reply that still reaches its addressee proves the unicast transport
/// actually delivers point-to-point — not via the gossip flood. Broadcasts
/// still ride gossip, so the warmup meshes the daemons and exchanges the
/// `PeerInfo` the sender's unicast dial resolves on.
#[test]
fn unicast_only_delivers_directed_reply() {
    let (creator, swarm) = Node::create_flags("uni-only", &[("--no-gossip-directed", "")]);
    let joiner = Node::join_flags(&swarm, "uni-only-b", &[("--no-gossip-directed", "")]);
    assert!(creator.wait_ready(&swarm), "creator never ready");
    assert!(joiner.wait_ready(&swarm), "joiner never ready");

    // Warmup broadcast (always gossip) meshes them + exchanges addresses.
    cli_message(&swarm, &creator.nickname, "warmup");
    assert!(
        wait_until(
            || joiner.count_from(&creator.nickname, "warmup"),
            1,
            MSG_TIMEOUT
        ) >= 1,
        "warmup broadcast never reached the joiner (mesh didn't form)"
    );

    // Directed reply: gossip won't carry it, so delivery ⇒ it went over unicast.
    // Under strict unicast-only the send *errors* until the sender has learned
    // the addressee's endpoint (its signed `PeerInfo`), so retry until it lands
    // — a success that reaches the joiner can only have gone point-to-point.
    let deadline = Instant::now() + MSG_TIMEOUT;
    while joiner.count_from(&creator.nickname, "unicast-only hello") == 0 {
        let _ = cli_message_raw(&swarm, &creator.nickname, "warmup"); // keep the mesh live
        let _ = cli_msg_stdout(
            &swarm,
            &creator.nickname,
            "unicast-only hello",
            Some(&joiner.nickname),
        );
        assert!(
            Instant::now() < deadline,
            "directed reply never arrived under unicast-only — unicast failed to deliver"
        );
        std::thread::sleep(POLL);
    }
}

/// With `--no-unicast`, the point-to-point transport is off and every message
/// rides gossip (the pre-unicast behavior). A directed reply must still reach
/// its addressee — the safety-switch parity check.
#[test]
fn gossip_only_delivers_directed_reply() {
    let (creator, swarm) = Node::create_flags("gos-only", &[("--no-unicast", "")]);
    let joiner = Node::join_flags(&swarm, "gos-only-b", &[("--no-unicast", "")]);
    assert!(creator.wait_ready(&swarm), "creator never ready");
    assert!(joiner.wait_ready(&swarm), "joiner never ready");

    cli_message(&swarm, &creator.nickname, "warmup");
    assert!(
        wait_until(
            || joiner.count_from(&creator.nickname, "warmup"),
            1,
            MSG_TIMEOUT
        ) >= 1,
        "warmup never reached the joiner"
    );

    cli_msg_checked(
        &swarm,
        &creator.nickname,
        "gossip-only hello",
        Some(&joiner.nickname),
    );
    assert!(
        wait_until(
            || joiner.count_from(&creator.nickname, "gossip-only hello"),
            1,
            MSG_TIMEOUT
        ) >= 1,
        "directed reply never arrived under gossip-only"
    );
}

/// A broadcast notice is delivered like a msg but surfaces as
/// `type:"notice"` with the `(notice)` display marker — the field agents
/// key the never-auto-reply contract on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_notice_delivery_surfaces_as_notice() {
    let creator = InProcNode::create("netnotice").await;
    let mut joiner = InProcNode::join(&creator.swarm, "joiner-notice").await;

    creator.notice(None, "build green").await;

    assert!(
        joiner.wait_body("build green", MSG_TIMEOUT).await,
        "joiner never received the notice"
    );
    let notices: Vec<serde_json::Value> = joiner
        .json_events()
        .into_iter()
        .filter(|event| event["type"] == "notice")
        .collect();
    assert_eq!(notices.len(), 1, "expected exactly 1 notice");
    assert_eq!(notices[0]["event"], "message");
    assert_eq!(notices[0]["body"], "build green");
    assert!(notices[0]["reply"].is_null());
    assert_eq!(
        notices[0]["display"],
        format!("🐝️ `<{}>` (notice): build green", creator.nickname)
    );
}

/// A directed notice has the same privacy as a directed msg: surfaced only
/// by its addressee; an uninvolved peer relays without seeing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_directed_notice_only_visible_to_addressee() {
    let mut alpha = InProcNode::create("netnotdir").await;
    let mut beta = InProcNode::join(&alpha.swarm, "beta-notdir").await;
    let mut gamma = InProcNode::join(&alpha.swarm, "gamma-notdir").await;

    alpha.send("warm up the mesh").await;
    assert!(
        beta.wait_inbound(1, MSG_TIMEOUT).await && gamma.wait_inbound(1, MSG_TIMEOUT).await,
        "alpha's message never reached beta and gamma"
    );

    beta.notice(Some(&alpha.nickname), "directed notice").await;

    assert!(
        alpha.wait_body("directed notice", MSG_TIMEOUT).await,
        "alpha (the addressee) should see the directed notice but didn't"
    );
    assert!(
        !gamma
            .inbound()
            .iter()
            .any(|msg| msg.body.as_str() == "directed notice"),
        "gamma (uninvolved) should NOT see the directed notice but did"
    );
}

/// Two agents on the same swarm must each get a distinct IPC socket.
#[test]
fn test_ipc_socket_isolation() {
    let (creator, swarm) = Node::create();
    let joiner = Node::join(&swarm, "joiner-ipc");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");

    let prefix = agent_habilis_swarm::swarm_prefix(&swarm);
    let sockets: Vec<_> = fs::read_dir(format!("{RUNTIME_DIR}/{prefix}"))
        .expect("swarm runtime dir missing")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".sock"))
        .collect();

    assert!(
        sockets.len() >= 2,
        "expected ≥2 per-agent sockets for this swarm, got {}: {:?}",
        sockets.len(),
        sockets
            .iter()
            .map(fs::DirEntry::file_name)
            .collect::<Vec<_>>()
    );
}

/// Send from both sides and verify each receives the other's message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bidirectional_messaging() {
    let mut creator = InProcNode::create("netbidir").await;
    let mut joiner = InProcNode::join(&creator.swarm, "joiner-bidir").await;

    // Each side posts as itself — creator receives joiner's message
    // and vice versa.
    creator.send("first message").await;
    joiner.send("second message").await;

    assert!(
        joiner.wait_body("first message", MSG_TIMEOUT).await,
        "joiner never received the creator's message"
    );
    assert!(
        creator.wait_body("second message", MSG_TIMEOUT).await,
        "creator never received the joiner's message"
    );
}

/// Verify that unit-level JSON wire format tests pass.
/// The ext field and version checks are covered in `protocol::message::tests`.
/// This integration test confirms the CLI sends parseable SWARM 1.0 blocks.
/// One send surfaces twice: the sender's own stream echoes it (stream
/// self-parity) and the peer receives it.
#[test]
fn test_stdout_format_parseable() {
    let (creator, swarm) = Node::create();
    let joiner = Node::join(&swarm, "joiner-fmt");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");

    cli_message(&swarm, &creator.nickname, "format check message");

    let total = wait_total(|| creator.messages().len() + joiner.messages().len(), 2);
    assert_eq!(total, 2, "one parseable self-echo + one parseable delivery");

    let msgs: Vec<Msg> = creator
        .messages()
        .into_iter()
        .chain(joiner.messages())
        .collect();
    for msg in &msgs {
        assert_eq!(msg.body, "format check message");
    }
}

/// `ask` with no running server exits non-zero with a clear error message.
#[test]
fn test_no_server_error() {
    // All-`1` Base58 payload — valid charset, can't match a real swarm.
    let fake_swarm = "🐝1111111111111111111111111111111111111111111111111111111111111";
    let out = cli_message_raw(fake_swarm, "ghost-nick", "hello");
    assert!(
        !out.status.success(),
        "expected non-zero exit when no server is running"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("No active swarm server"),
        "expected 'No active swarm server' in stderr, got: {stderr}"
    );
}

/// A body over the single-message cap is transparently split and delivered;
/// only a body too large for even `MAX_MESSAGE_PARTS` parts is refused.
#[test]
fn test_oversize_body_splits_then_refuses_past_the_part_cap() {
    let (creator, swarm) = Node::create();
    let joiner = Node::join(&swarm, "joiner-mp");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");

    // Over the single-message cap: the daemon splits it into parts and the
    // receiver reassembles, so it surfaces once as the whole body on each
    // stream — the sender's self-echo and the peer's delivery.
    let body = "a".repeat(agent_habilis_swarm::MAX_MESSAGE_SIZE * 2);
    cli_message(&swarm, &creator.nickname, &body);
    let total = wait_total(|| creator.messages().len() + joiner.messages().len(), 2);
    assert_eq!(
        total, 2,
        "the multipart body surfaces exactly once per node"
    );
    let got: Vec<Msg> = creator
        .messages()
        .into_iter()
        .chain(joiner.messages())
        .collect();
    for msg in &got {
        assert_eq!(msg.body, body, "the reassembled body matches the original");
    }

    // Too large for the part cap: refused on the sender with a clear error.
    let huge = "a".repeat(agent_habilis_swarm::MAX_LOGICAL_BODY_BYTES);
    let out = cli_message_raw(&swarm, &creator.nickname, &huge);
    assert!(
        !out.status.success(),
        "a body past the part cap must be refused"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("too large"),
        "expected a too-large error in stderr, got: {stderr}"
    );
}

/// UTF-8 message bodies (accents, emoji, CJK) are accepted and delivered verbatim.
#[test]
fn test_utf8_body_round_trip() {
    let (creator, swarm) = Node::create();
    let joiner = Node::join(&swarm, "joiner-utf8");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");

    let body = "héllo 🐝 日本語";
    cli_message(&swarm, &creator.nickname, body);

    // One send surfaces twice: the sender's self-echo + the peer's delivery.
    let total = wait_total(|| creator.messages().len() + joiner.messages().len(), 2);
    assert_eq!(total, 2, "utf-8 message delivered verbatim on both streams");
    let msgs: Vec<Msg> = creator
        .messages()
        .into_iter()
        .chain(joiner.messages())
        .collect();
    for msg in &msgs {
        assert_eq!(msg.body, body);
    }
}

/// Control characters (other than tab/newline) in a body are rejected.
#[test]
fn test_control_char_body_rejected() {
    let (creator, swarm) = Node::create();
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");

    let out = cli_message_raw(&swarm, &creator.nickname, "bad\u{7}bell");
    assert!(
        !out.status.success(),
        "expected non-zero exit for control-char body"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("control characters"),
        "expected control-char rejection in stderr, got: {stderr}"
    );
}

/// When a peer joins, the other node receives a SWARM 1.0 'joined' presence block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_presence_block_delivery() {
    let mut creator = InProcNode::create("netpresence").await;
    let _joiner = InProcNode::join(&creator.swarm, "joiner-presence").await;

    // The joiner broadcasts a `joined` presence on connect; the
    // creator must surface it.
    assert!(
        creator
            .wait_presence("joiner-presence", true, MSG_TIMEOUT)
            .await,
        "creator never surfaced the joiner's 'joined' presence"
    );
}

/// Multiple concurrent `ask` calls all get distinct IDs and all arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_concurrent_asks() {
    const ASK_COUNT: usize = 5;
    let mut creator = InProcNode::create("netconc").await;
    let joiner = InProcNode::join(&creator.swarm, "joiner-concurrent").await;

    let mut ids = std::collections::HashSet::new();
    for index in 0..ASK_COUNT {
        let id = joiner
            .send(&format!("concurrent message {index}"))
            .await
            .to_string();
        ids.insert(id);
    }
    assert_eq!(
        ids.len(),
        ASK_COUNT,
        "expected {ASK_COUNT} distinct ids, got {}: {ids:?}",
        ids.len()
    );

    assert!(
        creator.wait_inbound(ASK_COUNT, MSG_TIMEOUT).await,
        "creator did not receive all {ASK_COUNT} messages"
    );
}

/// With three agents on the same swarm, `ask --nickname <n>` must post as the
/// specified agent. Without `--nickname`, `find_socket` picks whichever socket
/// `read_dir` returns first — non-deterministic when multiple sockets share the
/// same swarm prefix. This test documents that bug and will pass once `ask`
/// accepts `--nickname`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_ask_targets_specific_agent_three_peers() {
    let alpha = InProcNode::create("netpick3").await;
    let mut beta = InProcNode::join(&alpha.swarm, "beta-pick3").await;
    let mut gamma = InProcNode::join(&alpha.swarm, "gamma-pick3").await;

    // Each node posts a uniquely-tagged message as itself; the other
    // two must receive it with the *correct* author (each in-process
    // session owns its own nickname, so authorship can't be swapped —
    // the in-process analogue of the old find_socket bug).
    let alpha_nick = alpha.nickname.clone();
    alpha.send("tag-from-alpha").await;
    for node in [&mut beta, &mut gamma] {
        assert!(
            node.wait_body("tag-from-alpha", MSG_TIMEOUT).await,
            "alpha's message not delivered"
        );
        let msg = node
            .inbound()
            .into_iter()
            .find(|msg| msg.body.as_str() == "tag-from-alpha")
            .expect("alpha's message missing");
        assert_eq!(
            msg.author.to_string(),
            alpha_nick,
            "wrong author for alpha's message"
        );
    }

    beta.send("tag-from-beta").await;
    gamma.send("tag-from-gamma").await;
    assert!(
        beta.wait_body("tag-from-gamma", MSG_TIMEOUT).await,
        "gamma's message not delivered to beta"
    );
    assert!(
        gamma.wait_body("tag-from-beta", MSG_TIMEOUT).await,
        "beta's message not delivered to gamma"
    );
    assert_eq!(
        beta.inbound()
            .into_iter()
            .find(|msg| msg.body.as_str() == "tag-from-gamma")
            .expect("gamma msg missing on beta")
            .author
            .to_string(),
        gamma.nickname,
        "wrong author for gamma's message"
    );
}

/// Graceful shutdown: SIGINT fires the ctrl-c handler which broadcasts a `left`
/// presence message before exiting. We verify the creator receives the `left` event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_graceful_shutdown_handler_fires() {
    let mut creator = InProcNode::create("netshutdown").await;
    let joiner = InProcNode::join(&creator.swarm, "joiner-shutdown").await;

    // Let the mesh form so the `Left` broadcast has a live link.
    assert!(
        creator
            .wait_presence("joiner-shutdown", true, MSG_TIMEOUT)
            .await,
        "joiner never surfaced as joined before leaving"
    );

    // Graceful leave broadcasts a `Left` presence.
    joiner.leave().await;

    assert!(
        creator
            .wait_presence("joiner-shutdown", false, MSG_TIMEOUT)
            .await,
        "creator never received the joiner's 'left' presence"
    );
}

/// Interleaved joins and leaves: verify each peer's `joined` event arrives before
/// its `left` in the observer's log.
///
/// Sequence:
///   alpha joins -> beta joins -> alpha leaves -> gamma joins -> beta leaves -> gamma leaves
///
/// Each step waits for the observer to confirm the event before proceeding,
/// so the causal ordering is enforced by the test driver itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_interleaved_join_leave_order() {
    let mut observer = InProcNode::create("netorder").await;

    let alpha = InProcNode::join(&observer.swarm, "alpha-order").await;
    assert!(
        observer.wait_presence_count(true, 1, MSG_TIMEOUT).await,
        "alpha joined not received"
    );

    let beta = InProcNode::join(&observer.swarm, "beta-order").await;
    assert!(
        observer.wait_presence_count(true, 2, MSG_TIMEOUT).await,
        "beta joined not received"
    );

    alpha.leave().await;
    assert!(
        observer.wait_presence_count(false, 1, MSG_TIMEOUT).await,
        "alpha left not received"
    );

    let gamma = InProcNode::join(&observer.swarm, "gamma-order").await;
    assert!(
        observer.wait_presence_count(true, 3, MSG_TIMEOUT).await,
        "gamma joined not received"
    );

    beta.leave().await;
    assert!(
        observer.wait_presence_count(false, 2, MSG_TIMEOUT).await,
        "beta left not received"
    );

    gamma.leave().await;
    assert!(
        observer.wait_presence_count(false, 3, MSG_TIMEOUT).await,
        "gamma left not received"
    );

    assert!(
        observer.presence_count(true) >= 3,
        "expected >=3 joined, got {}",
        observer.presence_count(true)
    );
    assert!(
        observer.presence_count(false) >= 3,
        "expected >=3 left, got {}",
        observer.presence_count(false)
    );
}

/// `--public` is accepted and the node starts successfully.
#[test]
fn test_network_public_accepted() {
    let log = tmp_log("public");
    let file = File::create(&log).unwrap();
    let mut child = common::test_cmd()
        .args([
            "create",
            "--name",
            "pub-test",
            "--public",
            "--no-interactive",
        ])
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file))
        .spawn()
        .expect("failed to spawn create --public");

    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut found = false;
    while Instant::now() < deadline {
        let content = fs::read_to_string(&log).unwrap_or_default();
        // In public mode the node may take longer to get addresses,
        // but it should at least print the create lifecycle line
        // (`created #NAME and joined as <nick>`).
        if content.contains("created #") {
            found = true;
            break;
        }
        std::thread::sleep(POLL);
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&log);

    assert!(found, "create --public did not produce any output");
}

/// A catchable termination signal must remove the `--state-file` so the
/// shell statusline pill clears immediately on leave instead of lingering
/// until its staleness window expires. Covers SIGTERM (what a Monitor
/// `TaskStop` sends) and SIGHUP (a closing parent) — both must trip the
/// graceful `shutdown()` path that deletes the file. SIGKILL is excluded:
/// it is uncatchable, so the file legitimately survives it.
#[test]
fn test_state_file_removed_on_signal() {
    for signal in ["-TERM", "-HUP", "-INT"] {
        let log = tmp_log(&format!("statefile{signal}"));
        let file = File::create(&log).unwrap();
        let state_file = std::env::temp_dir().join(format!(
            "ahsw-statefile-test-{}-{signal}.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&state_file);

        let mut child = common::test_cmd()
            .args(["create", "--name", "statefile-test", "--no-interactive"])
            .arg("--state-file")
            .arg(&state_file)
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .spawn()
            .expect("failed to spawn create --state-file");

        // The daemon writes the state file once the node is up.
        let appear = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < appear && !state_file.exists() {
            std::thread::sleep(POLL);
        }
        assert!(
            state_file.exists(),
            "daemon never wrote the state file ({signal})\nlog:\n{}",
            fs::read_to_string(&log).unwrap_or_default()
        );

        let _ = Command::new("kill")
            .args([signal, &child.id().to_string()])
            .status();

        // The graceful shutdown removes the file (after a ~500ms Left
        // broadcast window); allow generous slack.
        let gone = Instant::now() + Duration::from_secs(5);
        while Instant::now() < gone && state_file.exists() {
            std::thread::sleep(POLL);
        }
        assert!(
            !state_file.exists(),
            "state file survived {signal} — statusline pill would linger"
        );

        let _ = child.wait();
        let _ = fs::remove_file(&log);
        let _ = fs::remove_file(&state_file);
    }
}

/// `ahsw ready --state-file PATH` is the CLI-fallback readiness gate: it blocks
/// until the daemon writing PATH flips the file's `ready` flag to true (set
/// only once the event loop is serving), then exits 0. This covers the gate
/// against an already-up daemon and asserts the file then carries `ready:true`
/// plus the minted identity the caller reads next.
#[test]
fn test_ready_gate_succeeds_when_serving() {
    let log = tmp_log("ready-before");
    let file = File::create(&log).unwrap();
    let state_file =
        std::env::temp_dir().join(format!("ahsw-ready-before-{}.json", std::process::id()));
    let _ = fs::remove_file(&state_file);

    let mut child = common::test_cmd()
        .args(["create", "--name", "ready-test", "--no-interactive"])
        .arg("--state-file")
        .arg(&state_file)
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file))
        .spawn()
        .expect("failed to spawn create --state-file");

    // Gate against the live daemon: exits 0 once it is serving.
    let status = common::test_cmd()
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "60"])
        .status()
        .expect("failed to run ahsw ready");
    assert!(
        status.success(),
        "ahsw ready should exit 0 against a serving daemon\nlog:\n{}",
        fs::read_to_string(&log).unwrap_or_default()
    );

    // The gate returning means the file is complete and serving — the caller
    // reads identity + ready:true from it.
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_file).unwrap()).unwrap();
    assert_eq!(parsed["ready"], true, "gate returned but ready is not true");
    assert_eq!(parsed["name"], "ready-test");
    assert!(
        parsed["swarm"]
            .as_str()
            .is_some_and(|swarm| swarm.starts_with("🐝"))
    );
    assert!(parsed["nickname"].as_str().is_some());

    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();
    let _ = fs::remove_file(&log);
    let _ = fs::remove_file(&state_file);
}

/// The race the gate exists for: `ahsw ready` is started *before* the daemon, so
/// the state file does not exist yet. The gate must block (file-appears, then
/// ready-flips) and still exit 0 once the daemon comes up and serves.
#[test]
fn test_ready_gate_waits_for_a_late_daemon() {
    let log = tmp_log("ready-after");
    let file = File::create(&log).unwrap();
    let state_file =
        std::env::temp_dir().join(format!("ahsw-ready-after-{}.json", std::process::id()));
    let _ = fs::remove_file(&state_file);

    // Start the gate first — nothing has written the file yet.
    let mut gate = common::test_cmd()
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "60"])
        .spawn()
        .expect("failed to spawn ahsw ready");

    // Launch the daemon a beat later, writing the same state file.
    std::thread::sleep(Duration::from_millis(500));
    let mut child = common::test_cmd()
        .args(["create", "--name", "ready-race", "--no-interactive"])
        .arg("--state-file")
        .arg(&state_file)
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file))
        .spawn()
        .expect("failed to spawn create --state-file");

    let status = gate.wait().expect("ahsw ready never exited");
    assert!(
        status.success(),
        "ahsw ready started before the daemon should still exit 0 once it serves\nlog:\n{}",
        fs::read_to_string(&log).unwrap_or_default()
    );

    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();
    let _ = fs::remove_file(&log);
    let _ = fs::remove_file(&state_file);
}

/// With no daemon ever writing the file, the gate must give up at the timeout
/// and exit non-zero (the `failed to {create,join} swarm` contract the skills
/// rely on).
#[test]
fn test_ready_gate_times_out_without_a_daemon() {
    let state_file = std::env::temp_dir().join(format!(
        "ahsw-ready-timeout-{}-never.json",
        std::process::id()
    ));
    let _ = fs::remove_file(&state_file);

    let status = common::test_cmd()
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "2"])
        .status()
        .expect("failed to run ahsw ready");
    assert!(
        !status.success(),
        "ahsw ready should exit non-zero when no daemon ever writes the state file"
    );
}

/// A stale `ready:true` left by a prior daemon killed with SIGKILL must NOT
/// satisfy the gate: the gate checks `last_updated` freshness, so an old
/// timestamp (no live daemon refreshing it) is rejected and the gate times
/// out. Without the freshness check this file would be a false-positive ready.
#[test]
fn test_ready_gate_rejects_a_stale_ready_file() {
    let state_file =
        std::env::temp_dir().join(format!("ahsw-ready-stale-{}.json", std::process::id()));
    // ready:true but last_updated far in the past (well beyond READY_FRESH_SECS).
    fs::write(
        &state_file,
        r#"{"last_updated":1000000000,"name":"stale","nickname":"old-nick","participant_count":1,"ready":true,"swarm":"🐝deadbeef"}"#,
    )
    .unwrap();

    let status = common::test_cmd()
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "2"])
        .status()
        .expect("failed to run ahsw ready");
    assert!(
        !status.success(),
        "ahsw ready must reject a stale ready:true file (last_updated too old) and time out"
    );
    let _ = fs::remove_file(&state_file);
}

/// `ahsw ready --output json` doubles as the identity read: on a fresh
/// `ready:true` file it prints `{swarm,name,nickname}` and exits 0, so a
/// fallback caller learns its own identity from the gate without parsing the
/// state file (or guessing its `${PPID}` name) itself.
#[test]
fn test_ready_gate_emits_identity_json_on_success() {
    let state_file =
        std::env::temp_dir().join(format!("ahsw-ready-json-{}.json", std::process::id()));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        &state_file,
        format!(
            r#"{{"last_updated":{now},"name":"cool-team","nickname":"calm-otter","participant_count":1,"ready":true,"swarm":"🐝deadbeef"}}"#
        ),
    )
    .unwrap();

    let output = common::test_cmd()
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "5", "--output", "json"])
        .output()
        .expect("failed to run ahsw ready");
    assert!(
        output.status.success(),
        "a fresh ready:true file should pass the gate"
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ready --output json prints a JSON object");
    assert_eq!(parsed["swarm"], "🐝deadbeef");
    assert_eq!(parsed["name"], "cool-team");
    assert_eq!(parsed["nickname"], "calm-otter");
    let _ = fs::remove_file(&state_file);
}

/// The subprocess wire contract for `ahsw notice`: the CLI accepts it, and
/// the poll record carries the pinned `type:"notice"` shape with the
/// `(notice)` display marker — what agents key the no-auto-reply contract on.
#[test]
fn test_notice_wire_contract_over_cli_and_poll() {
    let (creator, swarm) = Node::create();
    let joiner = Node::join(&swarm, "joiner-notice-cli");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");
    std::thread::sleep(Duration::from_secs(2));

    cli_notice(&swarm, &joiner.nickname, "ci green", None);

    // The sender's own echo lands in its poll buffer with the pinned shape.
    let polled = wait_until(
        || {
            let raw = cli_poll(&swarm, &joiner.nickname, None);
            let events: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();
            events
                .iter()
                .filter(|event| event["type"] == "notice")
                .count()
        },
        1,
        MSG_TIMEOUT,
    );
    assert_eq!(polled, 1, "notice never surfaced in poll");

    let raw = cli_poll(&swarm, &joiner.nickname, None);
    let events: Vec<serde_json::Value> = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("failed to parse poll JSON: {error}\nraw: {raw}"));
    let notice = events
        .iter()
        .find(|event| event["type"] == "notice")
        .expect("notice record present");
    assert!(
        notice["seq"].is_u64(),
        "poll record carries a seq: {notice}"
    );
    assert_eq!(notice["event"], "message");
    assert_eq!(notice["body"], "ci green");
    assert_eq!(notice["self"], true, "joiner authored it → self:true");
    assert_eq!(
        notice["display"],
        "🐝️ `<joiner-notice-cli>` (notice): ci green"
    );
}

/// The poll command retrieves buffered events from a running swarm process,
/// each carrying its surfacing `seq`. Calling poll with `--after <seq>` returns
/// only events surfaced after that seq. The records are the same shape the live
/// `--output json` stream emits (`event`/`type`/`display`/`self`), so a fallback
/// agent parses one shape whether it reads the stream or polls.
#[test]
fn test_poll_returns_messages() {
    let (creator, swarm) = Node::create();
    let joiner = Node::join(&swarm, "joiner-poll");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");

    // Wait for presence to settle.
    std::thread::sleep(Duration::from_secs(2));

    // Send two messages via joiner's IPC.
    cli_message(&swarm, &joiner.nickname, "hello from poll test");
    std::thread::sleep(Duration::from_millis(500));
    cli_message(&swarm, &joiner.nickname, "second message");
    std::thread::sleep(Duration::from_millis(500));

    // Poll all events from joiner's process.
    let all_json = cli_poll(&swarm, &joiner.nickname, None);
    let all: Vec<serde_json::Value> = serde_json::from_str(&all_json)
        .unwrap_or_else(|error| panic!("failed to parse poll JSON: {error}\nraw: {all_json}"));

    // Should have at least the 2 messages we sent (plus possible presence).
    let msg_bodies: Vec<&str> = all.iter().filter_map(|msg| msg["body"].as_str()).collect();
    assert!(
        msg_bodies.contains(&"hello from poll test"),
        "first message missing from poll: {msg_bodies:?}"
    );
    assert!(
        msg_bodies.contains(&"second message"),
        "second message missing from poll: {msg_bodies:?}"
    );

    // Stream-parity shape: each record carries a monotonic `seq`, and a `msg`
    // record carries the pre-built `display` and the `self` flag (the joiner
    // authored these, so `self` is true).
    let first = all
        .iter()
        .find(|event| event["body"].as_str() == Some("hello from poll test"))
        .expect("first message not found");
    assert!(
        first["seq"].is_u64(),
        "poll record must carry a seq: {first}"
    );
    assert_eq!(first["event"], "message");
    assert_eq!(first["type"], "msg");
    assert_eq!(first["display"], "🐝️ `<joiner-poll>`: hello from poll test");
    assert_eq!(first["self"], true, "joiner authored it → self:true");

    // Poll with `--after <seq>` of the first message → excludes it, keeps the
    // second (the unified seq cursor).
    let first_seq = first["seq"].as_u64().expect("seq is u64");
    let after_json = cli_poll(&swarm, &joiner.nickname, Some(&first_seq.to_string()));
    let after: Vec<serde_json::Value> = serde_json::from_str(&after_json).unwrap_or_else(|error| {
        panic!("failed to parse after-poll JSON: {error}\nraw: {after_json}")
    });
    let after_bodies: Vec<&str> = after
        .iter()
        .filter_map(|event| event["body"].as_str())
        .collect();
    assert!(
        !after_bodies.contains(&"hello from poll test"),
        "--after should exclude events at/before the referenced seq"
    );
    assert!(
        after_bodies.contains(&"second message"),
        "second message missing from --after poll: {after_bodies:?}"
    );
    // Every returned seq is strictly greater than the cursor.
    for event in &after {
        assert!(
            event["seq"].as_u64().expect("seq") > first_seq,
            "--after must only return events with seq > cursor: {event}"
        );
    }
}

/// Baseline a node's poll cursor to "now": a first full poll, then advance
/// past its newest seq — so a long-poll after it sees only new events.
fn poll_cursor(swarm: &str, nickname: &str) -> Option<String> {
    let baseline = cli_poll(swarm, nickname, None);
    let baseline: Vec<serde_json::Value> = serde_json::from_str(&baseline).unwrap();
    baseline
        .iter()
        .filter_map(|event| event["seq"].as_u64())
        .max()
        .map(|seq| seq.to_string())
}

/// The wire single-park contract: a raw `{"command":"poll",...,"long":true}`
/// with no new traffic is held for ~the daemon's park cap, then returns
/// exactly `[]`. Sent straight over the Unix socket — the `ahsw` client would
/// hide the empty return behind its `--long` re-issue loop.
#[test]
fn test_ipc_poll_long_park_times_out_empty() {
    let (creator, swarm) = Node::create_flags("itest", &[("--longpoll-max-ms", "1000")]);
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    std::thread::sleep(Duration::from_secs(2)); // presence settles

    let after = poll_cursor(&swarm, &creator.nickname);
    let cursor = after.map_or(String::new(), |seq| format!("\"after\":{seq},"));
    let line = format!("{{\"command\":\"poll\",\"swarm\":\"{swarm}\",{cursor}\"long\":true}}");
    let started = Instant::now();
    let resp = ipc_raw(&swarm, &creator.nickname, &line);
    let elapsed = started.elapsed();
    assert_eq!(resp, "[]", "park elapsed quietly → empty array");
    assert!(
        elapsed >= Duration::from_millis(700),
        "should have parked ~1s, took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "park honored the shrunk cap, took {elapsed:?}"
    );
}

/// `poll --long` blocks, then resolves promptly when a peer sends: the parked
/// read wakes on the event landing, not on any timeout.
#[test]
fn test_poll_long_resolves_on_traffic() {
    let (creator, swarm) = Node::create();
    let joiner = Node::join(&swarm, "joiner-long");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");
    std::thread::sleep(Duration::from_secs(2)); // presence settles

    let after = poll_cursor(&swarm, &joiner.nickname);

    // Have the creator send ~400ms into the blocking poll; it must return the
    // message well under the daemon's 60s park cap — proving it woke on
    // traffic rather than spinning to an empty timeout.
    let swarm_for_send = swarm.clone();
    let creator_nick = creator.nickname.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        cli_message(&swarm_for_send, &creator_nick, "via long-poll");
    });
    let (got, resolve_elapsed) = cli_poll_long(&swarm, &joiner.nickname, after.as_deref());
    sender.join().unwrap();
    let events: Vec<serde_json::Value> = serde_json::from_str(&got)
        .unwrap_or_else(|error| panic!("parse long-poll JSON: {error}\nraw: {got}"));
    assert!(
        events
            .iter()
            .any(|event| event["body"].as_str() == Some("via long-poll")),
        "long-poll returned the message: {got}"
    );
    assert!(
        resolve_elapsed < Duration::from_mins(1),
        "woke on traffic rather than spinning to the park cap, took {resolve_elapsed:?}"
    );
}

/// The CLI's `--long` re-issue loop survives empty parks: with the daemon's
/// park cap shrunk to 1s and the message arriving at ~2.5s, a single
/// `poll --long` invocation rides through at least two empty windows and
/// still delivers it.
#[test]
fn test_poll_long_loops_past_empty_parks() {
    let (creator, swarm) = Node::create_flags("itest", &[("--longpoll-max-ms", "1000")]);
    let joiner = Node::join_flags(&swarm, "joiner-loop", &[("--longpoll-max-ms", "1000")]);
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");
    std::thread::sleep(Duration::from_secs(2)); // presence settles

    let after = poll_cursor(&swarm, &joiner.nickname);

    let swarm_for_send = swarm.clone();
    let creator_nick = creator.nickname.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2500));
        cli_message(&swarm_for_send, &creator_nick, "past the parks");
    });
    let (got, elapsed) = cli_poll_long(&swarm, &joiner.nickname, after.as_deref());
    sender.join().unwrap();
    let events: Vec<serde_json::Value> = serde_json::from_str(&got)
        .unwrap_or_else(|error| panic!("parse long-poll JSON: {error}\nraw: {got}"));
    assert!(
        events
            .iter()
            .any(|event| event["body"].as_str() == Some("past the parks")),
        "the looped long-poll returned the message: {got}"
    );
    assert!(
        elapsed >= Duration::from_secs(2),
        "survived at least two empty 1s parks before the send, took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_mins(1),
        "resolved promptly once the message landed, took {elapsed:?}"
    );
}

/// `ahsw ping` is daemon-owned: the transient command arms a round over
/// IPC, the daemon broadcasts a probe, every peer auto-pongs, and the
/// originator emits a `ping_report` on its own output stream listing
/// each responder's RTT. The probe/pong never surface as chat. A short
/// `PING_WINDOW_SECS` keeps the round fast.
#[test]
fn test_ping_reports_peer_rtt() {
    let (creator, swarm) = Node::create_flags("itest", &[("--ping-window-secs", "2")]);
    let joiner = Node::join(&swarm, "ping-joiner");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");

    // Let the mesh + presence settle so the probe reaches the joiner.
    std::thread::sleep(Duration::from_secs(3));

    // Arm the round on the creator; its report lands on the creator's
    // own stream once the 2s window closes.
    cli_ping(&swarm, &creator.nickname);

    // Poll the creator's captured output for the report (window + margin).
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut online_line = None;
    let mut rtt_line = None;
    while Instant::now() < deadline && (online_line.is_none() || rtt_line.is_none()) {
        for line in creator.log_contents().lines() {
            if line.contains("online") {
                online_line = Some(line.to_string());
            }
            if line.contains(&joiner.nickname) && line.contains("ms") {
                rtt_line = Some(line.to_string());
            }
        }
        if online_line.is_none() || rtt_line.is_none() {
            std::thread::sleep(POLL);
        }
    }

    assert!(
        online_line.is_some(),
        "ping_report 'N/M online' summary never appeared\nlog tail:\n{}",
        creator.log_tail(15)
    );
    assert!(
        rtt_line.is_some(),
        "ping_report never listed the joiner's RTT\nlog tail:\n{}",
        creator.log_tail(15)
    );

    // The probe/pong must not surface as chat on the joiner's stream.
    assert!(
        !joiner
            .messages()
            .iter()
            .any(|msg| msg.body == "ping" || msg.body == "pong"),
        "ping/pong leaked as chat messages to the joiner"
    );
}

/// An empty swarm (every member, including the creator, has left) is
/// **not** dead: joining it must still succeed. The joiner becomes the
/// rendezvous via `ensure`, and peers that arrive later connect to it.
#[test]
fn test_join_empty_swarm_succeeds_and_reseeds() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // Create a swarm, then kill its only member — the swarm is empty.
    let (creator, swarm) = Node::create();
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    creator.sigint();
    drop(creator);
    std::thread::sleep(Duration::from_secs(2));

    // Joining the empty swarm must succeed (previously this hung until
    // a 30s timeout and exited non-zero).
    let first = Node::join(&swarm, "empty-joiner");
    assert!(
        first.wait_ready(&swarm),
        "joining an empty swarm should succeed, not hang\nlog tail:\n{}",
        first.log_tail(15),
    );

    // A peer arriving later must be able to connect to that joiner,
    // which is now the rendezvous.
    let second = Node::join(&swarm, "later-peer");
    assert!(
        second.wait_ready(&swarm),
        "later peer could not connect to the re-seeded swarm\nfirst log:\n{}\nsecond log:\n{}",
        first.log_tail(15),
        second.log_tail(15),
    );

    // And messages flow across the re-seeded mesh.
    let id = cli_message(&swarm, &first.nickname, "re-seeded hello");
    assert!(!id.is_empty(), "msg returned empty id");
    let total = wait_total(|| first.messages().len() + second.messages().len(), 1);
    assert!(
        total >= 1,
        "no message crossed the re-seeded swarm\nfirst: {:?}\nsecond: {:?}",
        first.messages(),
        second.messages(),
    );
}

/// The headline resilience guarantee: a swarm survives its creator's death
/// as long as any member is still up. Creator + bystander form a mesh; the
/// creator is hard-killed; a brand-new joiner must still bootstrap — proof
/// that the bystander took over the seed-derived rendezvous (private mode:
/// claim-if-free on the deterministic loopback port).
#[test]
fn test_join_after_creator_departed_with_surviving_member() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // 1. Creator + a bystander that will outlive it.
    let (creator, swarm) = Node::create();
    let bystander = Node::join(&swarm, "bystander");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(
        bystander.wait_ready(&swarm),
        "bystander socket never appeared"
    );

    // Confirm the mesh is actually live before we kill the creator.
    let pre_id = cli_message(&swarm, &creator.nickname, "pre-death ping");
    assert!(!pre_id.is_empty(), "msg returned empty id");
    let delivered = wait_total(|| creator.messages().len() + bystander.messages().len(), 1);
    assert!(delivered >= 1, "creator/bystander never meshed pre-death");

    // 2. Hard-kill the creator. It was the initial rendezvous beacon;
    //    the deterministic port is now free for the bystander to claim
    //    on its next heal tick (~HEAL_INTERVAL_SECS).
    creator.sigint();
    drop(creator);
    // Wait the full claim-if-free handoff floor before a fresh peer
    // bootstraps: the bystander needs ≥2 heal cycles to win the freed port
    // and stand its rendezvous up (see `RENDEZVOUS_HANDOFF`). 22s (~1 heal
    // tick) raced the migration and flaked under load.
    std::thread::sleep(RENDEZVOUS_HANDOFF);

    // 3. A brand-new joiner that never saw the creator. Its only
    //    bootstrap target is the seed-derived rendezvous id; reaching
    //    `ready` proves the bystander is now serving it.
    let latecomer = Node::join(&swarm, "latecomer");
    assert!(
        latecomer.wait_ready(&swarm),
        "latecomer could not join after creator death — rendezvous failover broke\nbystander log tail:\n{}\nlatecomer log tail:\n{}",
        bystander.log_tail(15),
        latecomer.log_tail(15),
    );

    // 4. And the mesh genuinely works end-to-end across the failover:
    //    a message from the latecomer reaches the bystander.
    let post_id = cli_message(&swarm, &latecomer.nickname, "post-death hello");
    assert!(!post_id.is_empty(), "latecomer msg returned empty id");
    let total = wait_total(
        || bystander.messages().len() + latecomer.messages().len(),
        1,
    );
    assert!(
        total >= 1,
        "latecomer joined but no message crossed the post-failover mesh\nbystander msgs: {:?}",
        bystander.messages(),
    );
}

/// Regression for the "first message after a post-departure join is
/// lost" bug. A cold joiner connects via the surviving member's
/// rendezvous; its very first broadcast (sent immediately after
/// `ready`, no settling delay) must reach the existing peer, and the
/// existing peer's broadcast must reach the joiner. The reverse
/// direction specifically exercises the re-announce-on-`NeighborUp`
/// path: without it, the joiner's lost first `PeerInfo` is never
/// resent and existing peers never integrate it.
#[test]
fn test_first_message_after_post_departure_join_is_delivered() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    let (creator, swarm) = Node::create();
    let bystander = Node::join(&swarm, "fm-bystander");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(
        bystander.wait_ready(&swarm),
        "bystander socket never appeared"
    );
    // Confirm the mesh is live before killing the creator.
    let _ = cli_message(&swarm, &creator.nickname, "warmup");
    assert!(
        wait_total(|| creator.messages().len() + bystander.messages().len(), 1) >= 1,
        "creator/bystander never meshed pre-death"
    );

    creator.sigint();
    drop(creator);
    // Wait the full claim-if-free handoff floor so the bystander has
    // actually claimed the freed rendezvous before the latecomer joins —
    // this test exercises post-departure-join *delivery*, not migration
    // speed, so the migration must complete first (see `RENDEZVOUS_HANDOFF`).
    // The old 22s (~1 heal tick) let the latecomer join mid-migration and
    // flaked under CI load.
    std::thread::sleep(RENDEZVOUS_HANDOFF);

    let joiner = Node::join(&swarm, "fm-joiner");
    assert!(
        joiner.wait_ready(&swarm),
        "joiner could not join after creator death\nbystander:\n{}\njoiner:\n{}",
        bystander.log_tail(15),
        joiner.log_tail(15),
    );

    // First broadcast, sent immediately after `ready` (the exact bug
    // trigger): joiner -> bystander. Post-disruption delivery, so it gets
    // `RECOVERY_TIMEOUT`, not the steady-state `MSG_TIMEOUT`: routing the
    // joiner's first message waits on its `NeighborUp` re-announce, which is
    // gated by the 15s heal cadence, so a join that just missed a heal tick
    // legitimately needs another cycle.
    let j2b_id = cli_message(&swarm, &joiner.nickname, "j2b first");
    assert!(!j2b_id.is_empty(), "joiner msg returned empty id");
    assert!(
        wait_until(
            || bystander.count_from(&joiner.nickname, "j2b first"),
            1,
            RECOVERY_TIMEOUT
        ) >= 1,
        "joiner's first message was NOT received by the existing peer\nbystander msgs: {:?}",
        bystander.messages(),
    );

    // Reverse direction: existing peer -> joiner. Fails if the joiner
    // was never integrated into the mesh (lost first PeerInfo, no
    // re-announce).
    let b2j_id = cli_message(&swarm, &bystander.nickname, "b2j first");
    assert!(!b2j_id.is_empty(), "bystander msg returned empty id");
    assert!(
        wait_until(
            || joiner.count_from(&bystander.nickname, "b2j first"),
            1,
            RECOVERY_TIMEOUT
        ) >= 1,
        "existing peer's message was NOT received by the joiner\njoiner msgs: {:?}",
        joiner.messages(),
    );
}

/// Join horizon: a peer that joins after history was taskd must
/// **not surface** that pre-join history (anti-entropy still relays it
/// at the wire for swarm-wide resilience — that is intentionally not
/// observable here; only the view is filtered). A message sent *after*
/// it joined must still arrive, proving the node is meshed and only
/// the horizon, not connectivity, hides the old messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_join_horizon_hides_pre_join_history() {
    let creator = InProcNode::create("nethorizon").await;
    let mut early = InProcNode::join(&creator.swarm, "jh-early").await;

    // History taskd *before* the late peer exists.
    for tag in ["hist-1", "hist-2", "hist-3"] {
        creator.send(tag).await;
    }
    for tag in ["hist-1", "hist-2", "hist-3"] {
        assert!(
            early.wait_body(tag, MSG_TIMEOUT).await,
            "history not delivered to the existing peer before the late join: {tag}"
        );
    }

    // Whole-second timestamps: keep the history strictly an earlier
    // second than the late join (off the 1-second boundary; the real
    // case is seconds-to-minutes old).
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut late = InProcNode::join(&creator.swarm, "jh-late").await;

    // Well over an anti-entropy cycle so a later "still zero" means the
    // horizon suppressed the backfill, not that it merely hadn't
    // arrived yet (anti-entropy still relays it at the wire — that is
    // intentionally not observable here; only the view is filtered).
    tokio::time::sleep(Duration::from_secs(25)).await;
    for tag in ["hist-1", "hist-2", "hist-3"] {
        assert_eq!(
            late.count_body(tag),
            0,
            "pre-join history {tag} was surfaced to the late joiner"
        );
    }

    // But it IS meshed: a message sent after it joined must surface.
    creator.send("post-join-live").await;
    assert!(
        late.wait_body("post-join-live", MSG_TIMEOUT).await,
        "post-join message not delivered to the late joiner — connectivity broken, not just horizon"
    );
}

// ── reliability tests ────────────────────────────────────────────────────────
//
// `ALIVE_TIMEOUT_SECS`/`SWEEP_INTERVAL_SECS` are env-overridable, so
// `SHORT_EVICT` collapses the ~90s eviction window to seconds.
// `HEAL_INTERVAL_SECS` is not — it is a fixed 15s `const`; shortening
// it empirically destabilises convergence (see `src/tuning.rs`). So a
// claim-if-free handoff still costs its real ~34s: that floor is
// irreducible and dominates these tests' runtime.

const SHORT_EVICT: [(&str, &str); 2] = [
    ("--alive-timeout-secs", "3"),
    ("--sweep-interval-secs", "1"),
];

/// Assert `receiver` records `body` from `sender` within `within`,
/// dumping the receiver's log tail on failure. `wait_until` is
/// adaptive — a healthy run returns the instant the message lands.
fn assert_received(receiver: &Node, sender: &str, body: &str, within: Duration) {
    assert!(
        wait_until(|| receiver.count_from(sender, body), 1, within) >= 1,
        "{} never received {body:?} from {sender}\n{}",
        receiver.nickname,
        receiver.log_tail(20),
    );
}

/// Ungraceful (`kill -9`) beacon death: no graceful `Left`, the OS
/// reaps it. Survivors must keep exchanging, and a fresh joiner must
/// still bootstrap — proving a survivor took over the seed-derived
/// rendezvous via real claim-if-free. Ungraceful death (crash / OOM /
/// power loss) is the production failure mode.
#[test]
fn test_creator_sigkill_independence() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // The claim-if-free handoff floor (see `RENDEZVOUS_HANDOFF`): >= 2 heal
    // cycles + margin for the claim and a cold joiner's connect.
    let handoff = RENDEZVOUS_HANDOFF;

    let (creator, swarm) = Node::create_flags("itest", &SHORT_EVICT);
    let alpha = Node::join_flags(&swarm, "ck-alpha", &SHORT_EVICT);
    let bravo = Node::join_flags(&swarm, "ck-bravo", &SHORT_EVICT);
    assert!(creator.wait_ready(&swarm), "creator never ready");
    assert!(alpha.wait_ready(&swarm), "alpha never ready");
    assert!(bravo.wait_ready(&swarm), "bravo never ready");
    let _ = cli_message(&swarm, &creator.nickname, "ck-base");
    assert_received(&alpha, &creator.nickname, "ck-base", MSG_TIMEOUT);
    assert_received(&bravo, &creator.nickname, "ck-base", MSG_TIMEOUT);

    // Silent vanish: peers learn only via the alive-timeout path.
    creator.kill();
    drop(creator);
    let _ = cli_message(&swarm, &alpha.nickname, "ck-survive");
    assert_received(&bravo, &alpha.nickname, "ck-survive", RECOVERY_TIMEOUT);

    // A brand-new joiner can only reach the swarm if a survivor now
    // serves the seed-derived rendezvous.
    std::thread::sleep(handoff);
    let charlie = Node::join_flags(&swarm, "ck-charlie", &SHORT_EVICT);
    assert!(
        charlie.wait_ready(&swarm),
        "fresh joiner could not bootstrap after creator SIGKILL\nalpha:\n{}\ncharlie:\n{}",
        alpha.log_tail(15),
        charlie.log_tail(20),
    );
    let _ = cli_message(&swarm, &charlie.nickname, "ck-newcomer");
    assert_received(&alpha, &charlie.nickname, "ck-newcomer", RECOVERY_TIMEOUT);
}

/// Reaps a child on drop — `std::process::Child` does not, so a test that
/// panics before its explicit kill would otherwise leak the process.
struct KillOnDrop(std::process::Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Orphan self-termination: when the agent that spawned the daemon is
/// hard-killed, the daemon is reparented (not killed) and would otherwise
/// linger in the swarm forever. The `getppid` watcher must notice the
/// reparent and exit through the graceful `left`-broadcasting path.
///
/// To orphan the daemon without killing the test, we launch it under a
/// throwaway shell that backgrounds it and then `exec`s `sleep` to stay
/// alive as its parent. Killing that shell (not the daemon) is what an
/// agent reinstall / `kill -9` does in production.
#[test]
fn test_orphaned_daemon_self_terminates() {
    let _serial = serial_guard();

    let (observer, swarm) = Node::create_named("itest-orphan");
    assert!(observer.wait_ready(&swarm), "observer never ready");

    // Background the joiner under a shell, record its pid, then `exec sleep`
    // so the shell's pid *is* the joiner's parent for the rest of its life.
    // A 200ms watch interval means orphaning is noticed in well under a second.
    let pid_file = tmp_log("orphan-pid");
    let joiner_log = tmp_log("orphan-joiner-out");
    let script = format!(
        "'{bin}' --log-dir '{dir}' join {swarm} --nickname orphan-joiner \
            --ppid-watch-interval-ms 200 --output json >'{out}' 2>&1 & \
         echo $! >'{pid}'; exec sleep 600",
        bin = bin().display(),
        dir = common::test_log_dir(),
        swarm = swarm,
        out = joiner_log.display(),
        pid = pid_file.display(),
    );
    // `std::process::Child` has no kill-on-drop, so guard the intermediate
    // shell: if an assertion panics before the explicit kill below, this drop
    // still reaps it (which orphans the daemon → the watcher terminates it).
    let mut parent = KillOnDrop(
        Command::new("sh")
            .arg("-c")
            .arg(&script)
            .spawn()
            .expect("failed to spawn the intermediate parent shell"),
    );

    // Wait until the joiner is meshed: its socket exists and the observer's
    // roster lists it. Only then does its graceful `left` have a live link.
    let joiner_sock = socket_path(&swarm, "orphan-joiner");
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline
        && !(std::path::Path::new(&joiner_sock).exists()
            && cli_peers(&swarm, &observer.nickname).contains("orphan-joiner"))
    {
        std::thread::sleep(POLL);
    }
    assert!(
        cli_peers(&swarm, &observer.nickname).contains("orphan-joiner"),
        "joiner never meshed with the observer\nobserver:\n{}",
        observer.log_tail(15),
    );

    let joiner_pid = fs::read_to_string(&pid_file)
        .expect("joiner pid file never written")
        .trim()
        .to_string();
    assert!(!joiner_pid.is_empty(), "joiner pid file was empty");

    // Orphan the daemon: kill its parent (the shell, now `sleep`), NOT the
    // daemon. The daemon reparents → `getppid` changes → it self-quits.
    let _ = Command::new("kill")
        .args(["-KILL", &parent.0.id().to_string()])
        .status();
    let _ = parent.0.wait();

    // (a) The daemon process is gone — the watcher fired and it exited.
    let pid_alive = || {
        Command::new("kill")
            .args(["-0", &joiner_pid])
            .status()
            .is_ok_and(|status| status.success())
    };
    let death_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < death_deadline && pid_alive() {
        std::thread::sleep(POLL);
    }
    assert!(
        !pid_alive(),
        "orphaned daemon (pid {joiner_pid}) did not exit after its parent died",
    );

    // (b) It left *gracefully*: the observer drops it from the roster fast.
    // A silent death would keep it until the 90s alive-timeout, far beyond
    // this window — so a quick drop proves the `left` broadcast was received.
    let drop_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < drop_deadline
        && cli_peers(&swarm, &observer.nickname).contains("orphan-joiner")
    {
        std::thread::sleep(POLL);
    }
    assert!(
        !cli_peers(&swarm, &observer.nickname).contains("orphan-joiner"),
        "observer never received the orphaned daemon's graceful 'left'\nobserver:\n{}",
        observer.log_tail(15),
    );

    let _ = fs::remove_file(&pid_file);
    let _ = fs::remove_file(&joiner_log);
}

/// Sleep/wake: `SIGSTOP` a peer past the (shortened) alive-timeout so
/// the swarm evicts it, then `SIGCONT` and assert the heal primitive
/// re-meshes it and traffic resumes.
#[test]
fn test_sleep_wake_heal_recovery() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // Past ALIVE_TIMEOUT_SECS + SWEEP_INTERVAL_SECS (+margin) so the
    // sweeper evicts the frozen peer.
    let asleep = Duration::from_secs(8);
    // One heal tick (fixed 15s `const`) + margin to re-mesh the woken
    // peer. Irreducible.
    let wake_settle = Duration::from_secs(18);

    let (creator, swarm) = Node::create_flags("itest", &SHORT_EVICT);
    let sleeper = Node::join_flags(&swarm, "sw-sleeper", &SHORT_EVICT);
    assert!(creator.wait_ready(&swarm), "creator never ready");
    assert!(sleeper.wait_ready(&swarm), "sleeper never ready");
    let _ = cli_message(&swarm, &creator.nickname, "sw-pre");
    assert_received(&sleeper, &creator.nickname, "sw-pre", MSG_TIMEOUT);

    sleeper.stop();
    std::thread::sleep(asleep);
    assert!(
        creator.log_contents().contains("went quiet"),
        "creator never surfaced the frozen peer going quiet\n{}",
        creator.log_tail(20),
    );

    sleeper.cont();
    std::thread::sleep(wake_settle);
    let _ = cli_message(&swarm, &creator.nickname, "sw-post");
    assert_received(&sleeper, &creator.nickname, "sw-post", RECOVERY_TIMEOUT);
}

/// Fixed-node-id reconnect must be *fast*. A `SIGSTOP`/`SIGCONT` peer
/// resumes in the **same** process with the **same** iroh endpoint id —
/// exactly the iroh-gossip#10 / psyche #25 / p2panda #695 hazard, where a
/// stale *accepted* connection on a reused id can force a multi-minute
/// timeout before the peer is re-admitted to the gossip overlay. Our
/// heal + resume re-bootstrap must sidestep that, so we probe the woken
/// peer *immediately* on `SIGCONT` and require delivery within a bound
/// comfortably below that stale-connection timeout. A regression (or an
/// iroh bump reintroducing the stall) blows the bound. The lenient
/// `test_sleep_wake_heal_recovery` proves recovery *happens*; this one
/// proves it is heal-bound, not timeout-bound. See
/// docs/iroh-ecosystem-research.md.
#[test]
fn test_fixed_id_reconnect_admits_fast() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // Past SHORT_EVICT (3+1s) + margin so the sleeper is evicted.
    let asleep = Duration::from_secs(8);
    // Above our re-mesh cost (~1-2 fixed 15s heal cycles + resume
    // re-bootstrap) yet far below iroh's minutes-long stale-connection
    // timeout: a pass means admission is heal-bound.
    let admit_bound = Duration::from_secs(50);

    let (creator, swarm) = Node::create_flags("itest", &SHORT_EVICT);
    let sleeper = Node::join_flags(&swarm, "fr-sleeper", &SHORT_EVICT);
    assert!(creator.wait_ready(&swarm), "creator never ready");
    assert!(sleeper.wait_ready(&swarm), "sleeper never ready");
    let _ = cli_message(&swarm, &creator.nickname, "fr-pre");
    assert_received(&sleeper, &creator.nickname, "fr-pre", MSG_TIMEOUT);

    sleeper.stop();
    std::thread::sleep(asleep);
    assert!(
        creator.log_contents().contains("went quiet"),
        "creator never evicted the frozen peer\n{}",
        creator.log_tail(20),
    );

    // Resume, then probe at once: the message is sent while the woken
    // peer is reconnecting with its original id. Receipt within the
    // bound proves fast re-admission, not a stale-connection stall.
    sleeper.cont();
    let _ = cli_message(&swarm, &creator.nickname, "fr-post");
    assert_received(&sleeper, &creator.nickname, "fr-post", admit_bound);
}

/// Resume hard re-bootstrap: a peer frozen *past the stall threshold*
/// (process throttle / sleep proxy) must take the hard recovery path
/// — reset `meshed`, re-assert the rendezvous hint, long probe — not
/// just the weak periodic heal that the old code relied on (and which
/// silently failed to rebuild a fully-collapsed mesh). Shortens
/// `HEAL_STALL_THRESHOLD_SECS` so an 8s `SIGSTOP` trips it, then
/// asserts both the hard-path log marker and that post-wake traffic
/// flows again.
#[test]
fn test_resume_triggers_hard_rebootstrap() {
    // SHORT_EVICT + a shortened stall threshold. The threshold MUST
    // exceed the fixed 15s `HEAL_INTERVAL_SECS` (else every normal
    // ~15s heal tick false-positives as a resume and the node hard-
    // reboots forever), and the freeze MUST exceed the threshold.
    // 20s threshold < 30s freeze satisfies both; production is 60s.
    const STALL_EVICT: [(&str, &str); 3] = [
        ("--alive-timeout-secs", "3"),
        ("--sweep-interval-secs", "1"),
        ("--heal-stall-threshold-secs", "20"),
    ];
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // > stall threshold (20s), > evict window (3+1s).
    let asleep = Duration::from_secs(30);
    // tokio burst-fires the missed heal tick on SIGCONT; this is
    // headroom for the hard path to run and log the marker.
    let wake_settle = Duration::from_secs(20);

    let (creator, swarm) = Node::create_flags("itest", &STALL_EVICT);
    let sleeper = Node::join_flags(&swarm, "rb-sleeper", &STALL_EVICT);
    assert!(creator.wait_ready(&swarm), "creator never ready");
    assert!(sleeper.wait_ready(&swarm), "sleeper never ready");
    let _ = cli_message(&swarm, &creator.nickname, "rb-pre");
    assert_received(&sleeper, &creator.nickname, "rb-pre", MSG_TIMEOUT);

    sleeper.stop();
    std::thread::sleep(asleep);
    sleeper.cont();
    std::thread::sleep(wake_settle);

    // The hard-path marker is a `tracing` warn — it lands in the
    // sink log (AHS_LOG_DIR), not the operator stdout/stderr capture.
    let trace = trace_log(&swarm, &sleeper.nickname);
    assert!(
        trace.contains("hard re-bootstrap edge"),
        "woken peer never took the hard re-bootstrap path\nsink tail:\n{}",
        trace.lines().rev().take(30).collect::<Vec<_>>().join("\n"),
    );
    // The hard edge also fires the rendezvous-independent re-bridge: the
    // sleeper linked the creator before freezing, so on resume it
    // re-dials it directly rather than relying solely on a rendezvous
    // graft that a stale connection (iroh-gossip#10) could stall.
    assert!(
        trace.contains("rendezvous-independent re-bridge"),
        "woken peer never re-dialed known peers on the hard edge\nsink tail:\n{}",
        trace.lines().rev().take(30).collect::<Vec<_>>().join("\n"),
    );
    let _ = cli_message(&swarm, &creator.nickname, "rb-post");
    assert_received(&sleeper, &creator.nickname, "rb-post", RECOVERY_TIMEOUT);
}

/// Anti-entropy backfill: a peer that briefly freezes — but stays a
/// member (`gap` << alive-timeout) — misses a post-join message. The
/// join-horizon does not hide it (it post-dates the join), so
/// anti-entropy digest task must reconcile the gap.
///
/// Not `SHORT_EVICT`: the peer must stay a member, so the production
/// alive-timeout is required. The irreducible cost is the fixed 10s
/// anti-entropy cycle; the adaptive probe pays only real latency.
#[test]
fn test_anti_entropy_set_convergence() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // Well under the ~90s alive-timeout: the peer stays a member.
    let gap = Duration::from_secs(25);
    // Several 10s anti-entropy cycles, plus a heal if the freeze
    // dropped the gossip link. Adaptive — paid only if needed.
    let reconcile = Duration::from_secs(90);

    let (creator, swarm) = Node::create();
    let alpha = Node::join(&swarm, "ae-alpha");
    let bravo = Node::join(&swarm, "ae-bravo");
    assert!(creator.wait_ready(&swarm), "creator never ready");
    assert!(alpha.wait_ready(&swarm), "alpha never ready");
    assert!(bravo.wait_ready(&swarm), "bravo never ready");

    // alpha misses this; bravo gets it. alpha stays a member.
    alpha.stop();
    let _ = cli_message(&swarm, &creator.nickname, "ae-gap");
    assert_received(&bravo, &creator.nickname, "ae-gap", MSG_TIMEOUT);
    std::thread::sleep(gap);
    alpha.cont();

    // Post-join message, so the horizon does not hide it: anti-entropy
    // must backfill it to the still-member peer.
    assert_received(&alpha, &creator.nickname, "ae-gap", reconcile);
}

/// Large-gap reconnect replication: with a shared history larger than the
/// old ~90-uuid digest overflow point, a peer that freezes and misses a
/// burst must converge to the **full** set when it resumes.
///
/// Regression for the digest overflow: pre-fix, a buffer past ~90 messages
/// made every node's `recent_ids` digest (a JSON array of 36-char uuid
/// strings, ~8 KB at 200) exceed the gossip cap and get silently dropped —
/// so anti-entropy stalled and the gap never reconciled. The compact,
/// windowed digest keeps every digest under the cap, so it recovers.
#[test]
fn test_large_gap_reconnect_replication() {
    // Comfortably past the old ~98-uuid overflow *before* the gap, so the
    // old string-array digest would have been dropped.
    const PRELUDE: usize = 120;
    const GAP: usize = 30;
    const TOTAL: usize = PRELUDE + GAP;
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // Production alive-timeout (no `SHORT_EVICT`) so alpha stays a member
    // while frozen; a faster max-resend so the deep backfill converges
    // inside the window.
    let envs = [("--antientropy-max-resend", "128")];

    let (creator, swarm) = Node::create_args("itest", &[], &envs);
    let alpha = Node::join_flags(&swarm, "lg-alpha", &envs);
    let bravo = Node::join_flags(&swarm, "lg-bravo", &envs);
    assert!(creator.wait_ready(&swarm), "creator never ready");
    assert!(alpha.wait_ready(&swarm), "alpha never ready");
    assert!(bravo.wait_ready(&swarm), "bravo never ready");

    let author = creator.nickname.clone();

    // Build a shared history past the overflow point; alpha gets it live.
    for index in 0..PRELUDE {
        let _ = cli_message_raw(&swarm, &creator.nickname, &format!("lg-{index}"));
    }
    let live = wait_until(
        || alpha.count_distinct_from(&author, "lg-"),
        PRELUDE,
        Duration::from_mins(1),
    );
    assert_eq!(live, PRELUDE, "alpha never received the live prelude");

    // Freeze alpha, then send the gap it must later recover.
    alpha.stop();
    std::thread::sleep(Duration::from_secs(2));
    for index in PRELUDE..TOTAL {
        let _ = cli_message_raw(&swarm, &creator.nickname, &format!("lg-{index}"));
    }
    // bravo (live) gets everything — confirms the burst actually went out
    // while alpha was frozen.
    let bravo_total = wait_until(
        || bravo.count_distinct_from(&author, "lg-"),
        TOTAL,
        Duration::from_mins(1),
    );
    assert_eq!(bravo_total, TOTAL, "bravo never received the full burst");

    // Hold the freeze past iroh's default direct-path idle timeout (15s) so
    // alpha's link dies and the gap is genuinely missed — recovery then must go
    // through anti-entropy rather than a buffered post-resume delivery.
    std::thread::sleep(Duration::from_secs(25));
    // Resume: anti-entropy must backfill the gap. Generous — a frozen link
    // re-meshes on the 15s heal tick, then digests reconcile over a few
    // 10s cycles.
    alpha.cont();
    let final_count = wait_until(
        || alpha.count_distinct_from(&author, "lg-"),
        TOTAL,
        Duration::from_secs(150),
    );
    assert_eq!(
        final_count,
        TOTAL,
        "alpha did not converge to the full set after reconnect (got {final_count}/{TOTAL})\n{}",
        alpha.log_tail(20),
    );
}

/// Deep **interior** gap: a peer holds older *and* newer messages but is
/// missing a middle slice, so its open newest window cannot cover the gap —
/// it is recovered only by the rolling **closed older** window. Exercises
/// that path end to end (the tail-loss `test_large_gap_reconnect_replication`
/// does not).
#[test]
fn test_interior_gap_recovered_via_rolling_window() {
    const OLD: usize = 80; // shared older history
    const GAP: usize = 40; // interior gap (frozen peer misses these)
    const TAIL: usize = 80; // newer tail
    const TOTAL: usize = OLD + GAP + TAIL;
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    let envs = [("--antientropy-max-resend", "128")];

    let (creator, swarm) = Node::create_args("itest", &[], &envs);
    let alpha = Node::join_flags(&swarm, "ig-alpha", &envs);
    let bravo = Node::join_flags(&swarm, "ig-bravo", &envs);
    assert!(
        creator.wait_ready(&swarm) && alpha.wait_ready(&swarm) && bravo.wait_ready(&swarm),
        "nodes never ready"
    );
    let author = creator.nickname.clone();

    let mut idx = 0usize;
    // OLD: shared older history; alpha gets it live.
    for _ in 0..OLD {
        let _ = cli_message_raw(&swarm, &creator.nickname, &format!("ig-{idx}"));
        idx += 1;
    }
    assert_eq!(
        wait_until(
            || alpha.count_distinct_from(&author, "ig-"),
            OLD,
            Duration::from_mins(1)
        ),
        OLD,
        "alpha missed the shared history"
    );
    // Freeze alpha, send the GAP — the interior slice alpha never sees live.
    alpha.stop();
    for _ in 0..GAP {
        let _ = cli_message_raw(&swarm, &creator.nickname, &format!("ig-{idx}"));
        idx += 1;
    }
    assert_eq!(
        wait_until(
            || bravo.count_distinct_from(&author, "ig-"),
            OLD + GAP,
            Duration::from_mins(1)
        ),
        OLD + GAP,
        "bravo missed the gap batch"
    );
    // Hold the freeze past iroh's default direct-path idle timeout (15s) so
    // alpha's link dies and it genuinely misses the gap (recoverable only via
    // anti-entropy, not a buffered post-resume delivery).
    std::thread::sleep(Duration::from_secs(25));
    // Resume and send the newer TAIL. alpha ends up holding OLD + TAIL with
    // the GAP strictly below its newest window, so the gap is recoverable
    // only via the rolling older window.
    alpha.cont();
    for _ in 0..TAIL {
        let _ = cli_message_raw(&swarm, &creator.nickname, &format!("ig-{idx}"));
        idx += 1;
    }
    let final_count = wait_until(
        || alpha.count_distinct_from(&author, "ig-"),
        TOTAL,
        Duration::from_secs(150),
    );
    assert_eq!(
        final_count,
        TOTAL,
        "interior gap not recovered via the rolling older window (got {final_count}/{TOTAL})\n{}",
        alpha.log_tail(20),
    );
}

/// Steady-state churn-free: once a swarm with a buffer larger than one
/// digest window is fully converged, anti-entropy must go quiet — no peer
/// keeps re-sending. A naive sub-window digest (advertising less than it
/// holds) would make peers perpetually re-send the remainder; the `[lo,hi]`
/// bounds prevent that. We assert the `resent` log count stops growing.
#[test]
fn test_steady_state_no_resend_churn() {
    const COUNT: usize = 150; // > one 70-id window ⇒ the rolling older window is active
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    let envs = [
        ("RUST_LOG", "agent_habilis_swarm::gossip=debug"),
        ("--log-max-bytes", "0"), // no rotation, so the full log is one file
    ];

    let (creator, swarm) = Node::create_args("itest", &[], &envs);
    let alpha = Node::join_flags(&swarm, "cf-alpha", &envs);
    let bravo = Node::join_flags(&swarm, "cf-bravo", &envs);
    assert!(
        creator.wait_ready(&swarm) && alpha.wait_ready(&swarm) && bravo.wait_ready(&swarm),
        "nodes never ready"
    );
    let author = creator.nickname.clone();
    for index in 0..COUNT {
        let _ = cli_message_raw(&swarm, &creator.nickname, &format!("cf-{index}"));
    }
    assert_eq!(
        wait_until(
            || alpha.count_distinct_from(&author, "cf-"),
            COUNT,
            Duration::from_mins(1)
        ),
        COUNT,
        "alpha never converged"
    );
    assert_eq!(
        wait_until(
            || bravo.count_distinct_from(&author, "cf-"),
            COUNT,
            Duration::from_mins(1)
        ),
        COUNT,
        "bravo never converged"
    );

    // NOTE: greps the debug line emitted by `handle_digest` in
    // `src/gossip/antientropy.rs` ("anti-entropy: resent …"). Keep the two
    // in sync — if that message is reworded, update this match.
    let resends = || -> usize {
        [&creator, &alpha, &bravo]
            .iter()
            .map(|node| node.log_contents().matches("anti-entropy: resent").count())
            .sum()
    };
    // Settle past the convergence-era resends.
    std::thread::sleep(Duration::from_secs(25));
    let before = resends();
    // Two more anti-entropy cycles in a now-converged swarm.
    std::thread::sleep(Duration::from_secs(25));
    let after = resends();
    assert_eq!(
        before, after,
        "a converged swarm kept re-sending (churn): {before} -> {after}"
    );
}

/// Multi-round throttled backfill: a per-round resend budget far smaller
/// than the gap forces recovery across several anti-entropy cycles (not one
/// burst). Proves the cumulative catch-up converges.
#[test]
fn test_multi_round_throttled_backfill() {
    const GAP: usize = 40;
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    let envs = [("--antientropy-max-resend", "5")]; // tiny budget ⇒ many rounds

    let (creator, swarm) = Node::create_args("itest", &[], &envs);
    let alpha = Node::join_flags(&swarm, "mr-alpha", &envs);
    let bravo = Node::join_flags(&swarm, "mr-bravo", &envs);
    assert!(
        creator.wait_ready(&swarm) && alpha.wait_ready(&swarm) && bravo.wait_ready(&swarm),
        "nodes never ready"
    );
    let author = creator.nickname.clone();

    alpha.stop();
    std::thread::sleep(Duration::from_secs(2));
    for index in 0..GAP {
        let _ = cli_message_raw(&swarm, &creator.nickname, &format!("mr-{index}"));
    }
    let _ = wait_until(
        || bravo.count_distinct_from(&author, "mr-"),
        GAP,
        Duration::from_mins(1),
    );
    // Hold the freeze past iroh's default direct-path idle timeout (15s) so the
    // gap is genuinely missed and recovered through anti-entropy's throttled resend.
    std::thread::sleep(Duration::from_secs(25));
    alpha.cont();
    let final_count = wait_until(
        || alpha.count_distinct_from(&author, "mr-"),
        GAP,
        Duration::from_secs(150),
    );
    assert_eq!(
        final_count,
        GAP,
        "throttled backfill did not complete over multiple rounds (got {final_count}/{GAP})\n{}",
        alpha.log_tail(20),
    );
}

// ── Key-identity model (Tier 1) ─────────────────────────────────────────

/// Two members deliberately share a display nickname. Identity is the
/// signing **key**, not the name, and self-echo is keyed on the pubkey — so
/// each must still see the *other's* same-named messages. Regression guard
/// for the nickname→pubkey self-echo fix (a nickname-keyed self-echo would
/// have each node silently drop the other as its own echo).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_nickname_peers_communicate() {
    let mut alpha = InProcNode::create_with_nick("samenick", "dup").await;
    let mut beta = InProcNode::join(&alpha.swarm, "dup").await;

    alpha.send("from-alpha").await;
    assert!(
        beta.wait_body("from-alpha", MSG_TIMEOUT).await,
        "beta never saw alpha's message despite the shared nickname"
    );
    beta.send("from-beta").await;
    assert!(
        alpha.wait_body("from-beta", MSG_TIMEOUT).await,
        "alpha never saw beta's message despite the shared nickname"
    );

    // The surfaced message carries the author's full Ed25519 key — the real,
    // distinguishing identity behind the shared cosmetic nickname.
    let from_alpha = beta
        .inbound()
        .into_iter()
        .find(|msg| msg.body.as_str() == "from-alpha")
        .expect("alpha's message is present");
    assert_eq!(
        from_alpha.pubkey.len(),
        64,
        "a surfaced message carries the author's full 32-byte key as hex"
    );

    alpha.leave().await;
    beta.leave().await;
}

/// The `--output json` `message` event exposes the author's full public key,
/// so an agent can key trust/disambiguation on the key rather than the
/// (non-unique) nickname.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_event_carries_full_pubkey() {
    let alpha = InProcNode::create("pubkeyjson").await;
    let mut beta = InProcNode::join(&alpha.swarm, "pk-beta").await;

    alpha.send("hi").await;
    assert!(
        beta.wait_body("hi", MSG_TIMEOUT).await,
        "message never arrived"
    );

    let events = beta.message_events();
    let event = events
        .iter()
        .find(|event| event["body"] == "hi")
        .expect("the message event was surfaced");
    let pubkey = event["pubkey"]
        .as_str()
        .expect("the JSON message event carries a `pubkey` field");
    assert_eq!(pubkey.len(), 64, "full Ed25519 public key as 64 hex chars");

    alpha.leave().await;
    beta.leave().await;
}

/// A nickname is never "burned": after a member leaves, a new member can
/// take the same display name and communicate normally (ephemeral keys mean
/// the name was never *claimed* — only the key is the identity).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nickname_reusable_after_peer_leaves() {
    let mut observer = InProcNode::create("reuse").await;
    let first = InProcNode::join(&observer.swarm, "ditto").await;

    first.send("first-here").await;
    assert!(
        observer.wait_body("first-here", MSG_TIMEOUT).await,
        "observer never saw the first member"
    );
    first.leave().await;

    // A brand-new member reuses the departed nickname.
    let second = InProcNode::join(&observer.swarm, "ditto").await;
    second.send("second-here").await;
    assert!(
        observer.wait_body("second-here", MSG_TIMEOUT).await,
        "a reused nickname could not communicate after the prior holder left"
    );

    observer.leave().await;
    second.leave().await;
}

// ── starvation watchdog (the roster-collapse fix) ─────────────────────────────

// `SHORT_EVICT` plus a 6s starvation threshold. The threshold is its own
// flag (not derived from the alive timeout) precisely so the short-evict
// profile used across this suite never arms the watchdog by accident;
// these tests opt in explicitly.
const STARVE_EVICT: [(&str, &str); 3] = [
    ("--alive-timeout-secs", "3"),
    ("--sweep-interval-secs", "1"),
    ("--starvation-threshold-secs", "6"),
];

/// The starvation watchdog must fire — loudly — when the only peer
/// vanishes ungracefully. This is the detection the 11h roster-collapse
/// soak lacked: zero inbound while peers are known must produce a
/// recovery attempt (re-bridge + re-announce) and a log/JSON signal,
/// not eternal silence. The survivor must also stay functional.
#[test]
fn test_starvation_watchdog_recovers_loudly() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // Threshold (6s) + at most one heal tick (fixed 15s) + margin.
    let detect = Duration::from_secs(45);

    let (creator, swarm) = Node::create_flags("itest", &STARVE_EVICT);
    let survivor = Node::join_flags(&swarm, "sv-alpha", &STARVE_EVICT);
    assert!(creator.wait_ready(&swarm), "creator never ready");
    assert!(survivor.wait_ready(&swarm), "survivor never ready");
    let _ = cli_message(&swarm, &creator.nickname, "sv-base");
    assert_received(&survivor, &creator.nickname, "sv-base", MSG_TIMEOUT);

    // Silent vanish: keepalives stop, the survivor's inbound goes quiet.
    creator.kill();
    drop(creator);
    assert!(
        wait_until(
            || usize::from(survivor.log_contents().contains("mesh starvation")),
            1,
            detect,
        ) >= 1,
        "watchdog never fired after the only peer vanished\n{}",
        survivor.log_tail(25),
    );
    // Degraded, not broken: the IPC plane still accepts a send (it is
    // buffered until traffic proves the mesh again).
    let _ = cli_msg_checked(&swarm, &survivor.nickname, "sv-after", None);
}

/// False-positive guard: a lone creator is alone by construction — it
/// never announced into a mesh of real peers and knows nobody to
/// re-dial — so the watchdog must stay silent no matter how long it
/// idles past the threshold.
#[test]
fn test_lone_creator_never_trips_starvation() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    let (creator, swarm) = Node::create_flags("itest", &STARVE_EVICT);
    assert!(creator.wait_ready(&swarm), "creator never ready");
    // Threshold (6s) + two heal ticks (15s each) of opportunity to misfire.
    std::thread::sleep(Duration::from_secs(32));
    assert!(
        !creator.log_contents().contains("mesh starvation"),
        "lone creator false-tripped the starvation watchdog\n{}",
        creator.log_tail(25),
    );
}

/// The 2026-05-31 roster-collapse, mechanized: a 5-node swarm at
/// `--active-view-capacity 2` (the partial-mesh churn regime) put
/// through SIGSTOP/SIGCONT flap rounds. Pre-fix, a node could end up
/// with an empty roster and phantom links forever — silent message
/// loss. Post-fix (link truth + starvation watchdog), every node must
/// deliver again once the storm passes.
#[test]
fn test_flap_storm_all_rosters_recover() {
    const CAP2_STARVE: [(&str, &str); 4] = [
        ("--alive-timeout-secs", "3"),
        ("--sweep-interval-secs", "1"),
        ("--starvation-threshold-secs", "6"),
        ("--active-view-capacity", "2"),
    ];
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // Stop window: past the 3+1s evict so victims get swept; resume gap
    // long enough for partial re-meshing before the next round hits.
    let stop_window = Duration::from_secs(8);
    let resume_gap = Duration::from_secs(5);
    // Recovery bound: starvation threshold (6s) + a couple of fixed 15s
    // heal ticks for re-bridge/re-announce to propagate, plus margin.
    let recover = RECOVERY_TIMEOUT;

    let (creator, swarm) = Node::create_flags("itest", &CAP2_STARVE);
    let joiners: Vec<Node> = (0..4)
        .map(|index| Node::join_flags(&swarm, &format!("fs-{index}"), &CAP2_STARVE))
        .collect();
    assert!(creator.wait_ready(&swarm), "creator never ready");
    for joiner in &joiners {
        assert!(joiner.wait_ready(&swarm), "{} never ready", joiner.nickname);
    }
    // Baseline delivery gets the same generous bound as the post-storm
    // probe: at cap 2 even a healthy broadcast is partial-mesh-routed
    // (multi-hop, convergence-dependent), so the standard 30s message
    // timeout is occasionally short here. Adaptive — healthy runs pass
    // in seconds.
    let _ = cli_message(&swarm, &creator.nickname, "fs-base");
    for joiner in &joiners {
        assert_received(joiner, &creator.nickname, "fs-base", recover);
    }

    // Three flap rounds, two victims each, rotating across all five
    // nodes — the same storm shape that wedged the soak and the manual
    // repro (roster_len=0 with phantom links, delivery frozen).
    let all: Vec<&Node> = std::iter::once(&creator).chain(joiners.iter()).collect();
    for round in 0..3usize {
        let first = all[round % all.len()];
        let second = all[(round + 2) % all.len()];
        first.stop();
        second.stop();
        std::thread::sleep(stop_window);
        first.cont();
        second.cont();
        std::thread::sleep(resume_gap);
    }

    // Settle, then the invariant: a fresh broadcast reaches EVERY node.
    std::thread::sleep(Duration::from_secs(10));
    let _ = cli_message(&swarm, &creator.nickname, "fs-probe");
    for joiner in &joiners {
        assert_received(joiner, &creator.nickname, "fs-probe", recover);
    }
}

// ── leave / session (session-scope daemon discovery) ─────────────────────────

/// Spawn a create daemon with `--output json` and block until its `ready`
/// event appears on stdout, returning the child plus its minted identity.
/// The default state-file location (under `RUNTIME_DIR`) is what `leave` /
/// `session` discover, so no `--state-file` override here.
#[expect(
    clippy::zombie_processes,
    reason = "the child is returned; every caller kills and waits it"
)]
fn spawn_discoverable_daemon(name: &str) -> (std::process::Child, PathBuf, String, String) {
    let log = tmp_log(&format!("leave-{name}"));
    let file = File::create(&log).unwrap();
    let mut child = common::test_cmd()
        .args([
            "create",
            "--name",
            name,
            "--no-interactive",
            "--output",
            "json",
        ])
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file))
        .spawn()
        .expect("failed to spawn create");
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        if let Some((swarm, nickname)) = ready_identity(&log) {
            return (child, log, swarm, nickname);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "daemon never emitted ready\nlog:\n{}",
                fs::read_to_string(&log).unwrap_or_default()
            );
        }
        std::thread::sleep(POLL);
    }
}

fn ready_identity(log: &std::path::Path) -> Option<(String, String)> {
    let content = fs::read_to_string(log).ok()?;
    let ready = content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["event"] == "ready")?;
    Some((
        ready["swarm"].as_str()?.to_owned(),
        ready["nickname"].as_str()?.to_owned(),
    ))
}

fn default_state_file(swarm: &str, nickname: &str) -> PathBuf {
    let prefix = agent_habilis_swarm::swarm_prefix(swarm);
    PathBuf::from(RUNTIME_DIR)
        .join(prefix)
        .join(format!("{nickname}.state.json"))
}

/// The state file must carry the daemon's own pid — what lets `leave` /
/// `session` map the file back to a live process and to the agent session
/// that spawned it.
#[test]
fn state_file_carries_daemon_pid() {
    let (mut child, log, swarm, nickname) = spawn_discoverable_daemon("leave-pid");
    let state_file = default_state_file(&swarm, &nickname);
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_file).unwrap()).unwrap();
    assert_eq!(parsed["pid"], child.id());

    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();
    let _ = fs::remove_file(&log);
}

/// `ahsw leave <🐝id>` (explicit target) stops exactly that swarm's local
/// daemon — the state file disappears (proof of the graceful shutdown path)
/// — and leaves an unrelated daemon untouched.
#[test]
fn leave_explicit_target_stops_only_that_swarm() {
    let (mut victim, victim_log, victim_swarm, victim_nick) =
        spawn_discoverable_daemon("leave-victim");
    let (mut bystander, bystander_log, bystander_swarm, bystander_nick) =
        spawn_discoverable_daemon("leave-bystander");

    let out = common::test_cmd()
        .args(["leave", &victim_swarm, "--output", "json"])
        .output()
        .expect("failed to run ahsw leave");
    assert!(out.status.success(), "leave failed: {out:?}");
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let left = report["left"].as_array().unwrap();
    assert_eq!(left.len(), 1, "expected exactly the victim: {report}");
    assert_eq!(left[0]["swarm"], victim_swarm.as_str());
    assert_eq!(left[0]["confirmed"], true);
    assert!(
        !default_state_file(&victim_swarm, &victim_nick).exists(),
        "victim state file survived leave"
    );
    assert!(
        default_state_file(&bystander_swarm, &bystander_nick).exists(),
        "bystander state file vanished — leave over-matched"
    );

    let _ = victim.wait();
    let _ = Command::new("kill")
        .args(["-TERM", &bystander.id().to_string()])
        .status();
    let _ = bystander.wait();
    let _ = fs::remove_file(&victim_log);
    let _ = fs::remove_file(&bystander_log);
}

/// Session scope end to end: a daemon spawned under a decoy "agent" shell is
/// owned by that shell's pid. `ahsw session --session-pid <shell>` reports it
/// without touching it; `ahsw leave --session-pid <shell>` stops it. Daemons
/// belonging to other tests (children of this test binary, not of the decoy
/// shell) must never match.
#[test]
fn leave_session_scope_via_decoy_parent() {
    let _serial = serial_guard();

    let log = tmp_log("leave-decoy");
    // The trailing `:` defeats the shell's exec-of-last-command optimization,
    // keeping the shell alive as the daemon's parent — the ancestry link the
    // session scope matches on.
    let mut decoy = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "{} --log-dir {} create --name leave-decoy --no-interactive --output json > {} 2>&1; :",
            bin().display(),
            common::test_log_dir(),
            log.display(),
        ))
        .spawn()
        .expect("failed to spawn decoy shell");
    let decoy_pid = decoy.id().to_string();

    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while ready_identity(&log).is_none() {
        assert!(
            Instant::now() < deadline,
            "decoy daemon never emitted ready\nlog:\n{}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        std::thread::sleep(POLL);
    }
    let (swarm, nickname) = ready_identity(&log).unwrap();

    // Read-only probe: reports the decoy's daemon, does not stop it.
    let out = common::test_cmd()
        .args(["session", "--session-pid", &decoy_pid, "--output", "json"])
        .output()
        .expect("failed to run ahsw session");
    assert!(out.status.success(), "session failed: {out:?}");
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let sessions = report["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "expected exactly the decoy: {report}");
    assert_eq!(sessions[0]["swarm"], swarm.as_str());
    assert_eq!(sessions[0]["nickname"], nickname.as_str());
    assert!(
        default_state_file(&swarm, &nickname).exists(),
        "session (read-only) stopped the daemon"
    );

    let leave_out = common::test_cmd()
        .args(["leave", "--session-pid", &decoy_pid, "--output", "json"])
        .output()
        .expect("failed to run ahsw leave");
    assert!(leave_out.status.success(), "leave failed: {leave_out:?}");
    let leave_report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&leave_out.stdout).trim()).unwrap();
    let left = leave_report["left"].as_array().unwrap();
    assert_eq!(left.len(), 1, "expected exactly the decoy: {leave_report}");
    assert_eq!(left[0]["swarm"], swarm.as_str());
    assert_eq!(left[0]["confirmed"], true);
    assert!(!default_state_file(&swarm, &nickname).exists());

    let _ = decoy.wait();
    let _ = fs::remove_file(&log);
}

/// A session that owns no daemon gets an empty `left` and exit 0 — never an
/// error, and never someone else's daemons.
#[test]
fn leave_nothing_owned_is_a_clean_noop() {
    let mut idle = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");

    let out = common::test_cmd()
        .args([
            "leave",
            "--session-pid",
            &idle.id().to_string(),
            "--output",
            "json",
        ])
        .output()
        .expect("failed to run ahsw leave");
    assert!(out.status.success(), "leave should exit 0 on a no-op");
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["left"].as_array().unwrap().len(), 0);

    let _ = idle.kill();
    let _ = idle.wait();
}
