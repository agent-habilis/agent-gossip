//! Integration tests for the gossip network.
//!
//! Each test spawns real `ahs` processes, exercises the network,
//! and asserts on what each node actually received. Tests are independent —
//! each creates its own swarm so IPC sockets never collide.
//!
//! Run `cargo build --release` first for faster crypto (shorter connect times).
mod common;

use std::fs::{self, File};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agent_habilis_swarm::RATE_LIMIT_PER_MIN;
use common::{
    CONNECT_TIMEOUT, InProcNode, MSG_TIMEOUT, Msg, Node, POLL, RECOVERY_TIMEOUT, SOCKET_DIR, bin,
    cli_message, cli_message_raw, cli_peers, cli_ping, cli_poll, cli_poll_wait, serial_guard,
    socket_path, tmp_log, trace_log, wait_total, wait_until,
};

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

/// Durable state log, live propagation: a creator appends state events; a
/// meshed joiner converges to the same derived state via gossip. State rides
/// the same topic but its own un-pruned log, surfaced through `state_snapshot`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_state_log_propagates_to_a_peer() {
    let creator = InProcNode::create("netstate").await;
    let joiner = InProcNode::join(&creator.swarm, "joiner-state").await;

    // Mesh first (a delivered message proves the link), so the state events
    // broadcast onto a live overlay rather than the unmeshed buffer.
    creator.send("link").await;

    creator.append_state("alpha").await;
    creator.append_state("beta").await;

    let want = vec!["alpha".to_string(), "beta".to_string()];
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut got = joiner.state_sorted().await;
    while got != want && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        got = joiner.state_sorted().await;
    }
    assert_eq!(
        got, want,
        "joiner never converged to the creator's state log"
    );
    // The author holds its own events too (gossip never echoes to self).
    assert_eq!(creator.state_sorted().await, want);
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

    creator.append_state("alpha").await;
    creator.append_state("beta").await;

    let want = vec!["alpha".to_string(), "beta".to_string()];
    // Confirm the live path first.
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut early_got = early.state_sorted().await;
    while early_got != want && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        early_got = early.state_sorted().await;
    }
    assert_eq!(early_got, want, "early peer never got the live state");

    // The late joiner arrives after all state traffic; only anti-entropy can
    // backfill it (within an antientropy interval once it advertises its set).
    let late = InProcNode::join(&creator.swarm, "late-state").await;
    let late_deadline = Instant::now() + RECOVERY_TIMEOUT;
    let mut late_got = late.state_sorted().await;
    while late_got != want && Instant::now() < late_deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        late_got = late.state_sorted().await;
    }
    assert_eq!(
        late_got, want,
        "late joiner never backfilled state via anti-entropy"
    );
}

/// Sender-side rate limiting, symmetric with the receiver: a node may
/// emit up to its per-author quota, then its own sends are dropped
/// (`Ok(None)`) rather than broadcast. Mirrors the receiver-side
/// `rate_limiter_drops_excess_messages_from_flooding_peer`. One node is
/// enough — the limiter is checked before any mesh interaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_rate_limits_own_excess_messages() {
    let node = InProcNode::create("net-send-rl").await;

    // The token bucket's depth equals the per-minute quota, so exactly
    // that many back-to-back sends are admitted.
    for index in 0..RATE_LIMIT_PER_MIN {
        let outcome = node.try_send(&format!("msg {index}")).await;
        assert!(
            matches!(outcome, Ok(Some(_))),
            "send {index} within quota should be admitted, got {outcome:?}"
        );
    }
    // The next own send is rate-limited: a deliberate drop, not an error.
    let dropped = node.try_send("one too many").await;
    assert!(
        matches!(dropped, Ok(None)),
        "send past the quota should be dropped as Ok(None), got {dropped:?}"
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

/// Two agents on the same swarm must each get a distinct IPC socket.
#[test]
fn test_ipc_socket_isolation() {
    let (creator, swarm) = Node::create();
    let joiner = Node::join(&swarm, "joiner-ipc");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");

    let prefix = agent_habilis_swarm::swarm_prefix(&swarm);
    let sockets: Vec<_> = fs::read_dir(SOCKET_DIR)
        .expect("socket dir missing")
        .flatten()
        .filter(|entry| {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            file_name.starts_with(prefix.as_str()) && file_name.ends_with(".sock")
        })
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
#[test]
fn test_stdout_format_parseable() {
    let (creator, swarm) = Node::create();
    let joiner = Node::join(&swarm, "joiner-fmt");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");

    cli_message(&swarm, &creator.nickname, "format check message");

    let total = wait_total(|| creator.messages().len() + joiner.messages().len(), 1);
    assert_eq!(total, 1, "message should be parseable and delivered");

    let msgs: Vec<Msg> = creator
        .messages()
        .into_iter()
        .chain(joiner.messages())
        .collect();
    assert_eq!(msgs[0].body, "format check message");
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

/// Messages over 16 KB are rejected with a clear error.
#[test]
fn test_message_size_limit() {
    let (creator, swarm) = Node::create();
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");

    // A body at the cap, plus the JSON envelope, exceeds the serialized
    // limit — rejected on the sender (a clear error, not a silent drop).
    let body = "a".repeat(agent_habilis_swarm::MAX_MESSAGE_SIZE);
    let out = cli_message_raw(&swarm, &creator.nickname, &body);
    assert!(
        !out.status.success(),
        "expected non-zero exit for oversized message"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("too large"),
        "expected size-limit error in stderr, got: {stderr}"
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

    let total = wait_total(|| creator.messages().len() + joiner.messages().len(), 1);
    assert_eq!(total, 1, "utf-8 message should be delivered");
    let msgs: Vec<Msg> = creator
        .messages()
        .into_iter()
        .chain(joiner.messages())
        .collect();
    assert_eq!(msgs[0].body, body);
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
            "ahs-statefile-test-{}-{signal}.json",
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

/// `ahs ready --state-file PATH` is the CLI-fallback readiness gate: it blocks
/// until the daemon writing PATH flips the file's `ready` flag to true (set
/// only once the event loop is serving), then exits 0. This covers the gate
/// against an already-up daemon and asserts the file then carries `ready:true`
/// plus the minted identity the caller reads next.
#[test]
fn test_ready_gate_succeeds_when_serving() {
    let log = tmp_log("ready-before");
    let file = File::create(&log).unwrap();
    let state_file =
        std::env::temp_dir().join(format!("ahs-ready-before-{}.json", std::process::id()));
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
        .expect("failed to run ahs ready");
    assert!(
        status.success(),
        "ahs ready should exit 0 against a serving daemon\nlog:\n{}",
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

/// The race the gate exists for: `ahs ready` is started *before* the daemon, so
/// the state file does not exist yet. The gate must block (file-appears, then
/// ready-flips) and still exit 0 once the daemon comes up and serves.
#[test]
fn test_ready_gate_waits_for_a_late_daemon() {
    let log = tmp_log("ready-after");
    let file = File::create(&log).unwrap();
    let state_file =
        std::env::temp_dir().join(format!("ahs-ready-after-{}.json", std::process::id()));
    let _ = fs::remove_file(&state_file);

    // Start the gate first — nothing has written the file yet.
    let mut gate = common::test_cmd()
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "60"])
        .spawn()
        .expect("failed to spawn ahs ready");

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

    let status = gate.wait().expect("ahs ready never exited");
    assert!(
        status.success(),
        "ahs ready started before the daemon should still exit 0 once it serves\nlog:\n{}",
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
        "ahs-ready-timeout-{}-never.json",
        std::process::id()
    ));
    let _ = fs::remove_file(&state_file);

    let status = common::test_cmd()
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "2"])
        .status()
        .expect("failed to run ahs ready");
    assert!(
        !status.success(),
        "ahs ready should exit non-zero when no daemon ever writes the state file"
    );
}

/// A stale `ready:true` left by a prior daemon killed with SIGKILL must NOT
/// satisfy the gate: the gate checks `last_updated` freshness, so an old
/// timestamp (no live daemon refreshing it) is rejected and the gate times
/// out. Without the freshness check this file would be a false-positive ready.
#[test]
fn test_ready_gate_rejects_a_stale_ready_file() {
    let state_file =
        std::env::temp_dir().join(format!("ahs-ready-stale-{}.json", std::process::id()));
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
        .expect("failed to run ahs ready");
    assert!(
        !status.success(),
        "ahs ready must reject a stale ready:true file (last_updated too old) and time out"
    );
    let _ = fs::remove_file(&state_file);
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

/// `poll --wait <ms>` long-polls: with no new traffic it blocks for ~the wait
/// then returns an empty array; when a peer sends mid-wait it returns promptly,
/// well before the timeout. The daemon never blocks — only the held call waits.
#[test]
fn test_poll_wait_blocks_then_resolves_and_times_out() {
    let (creator, swarm) = Node::create();
    let joiner = Node::join(&swarm, "joiner-wait");
    assert!(creator.wait_ready(&swarm), "creator socket never appeared");
    assert!(joiner.wait_ready(&swarm), "joiner socket never appeared");
    std::thread::sleep(Duration::from_secs(2)); // presence settles

    // Baseline the joiner's cursor to "now" so the waits below see only new
    // events: a first full poll, then advance past its newest seq.
    let baseline = cli_poll(&swarm, &joiner.nickname, None);
    let baseline: Vec<serde_json::Value> = serde_json::from_str(&baseline).unwrap();
    let last_seq = baseline
        .iter()
        .filter_map(|event| event["seq"].as_u64())
        .max();
    let after = last_seq.map(|seq| seq.to_string());

    // (1) Timeout: no traffic, a 1s wait returns `[]` after ~blocking ~1s.
    let (empty, timeout_elapsed) =
        cli_poll_wait(&swarm, &joiner.nickname, after.as_deref(), "1000");
    assert_eq!(empty, "[]", "no traffic → empty array");
    assert!(
        timeout_elapsed >= Duration::from_millis(700),
        "should have blocked ~1s, took {timeout_elapsed:?}"
    );

    // (2) Resolves on traffic: start a blocking 15s wait, have the creator
    // send ~400ms in; the poll must return the message well under the timeout.
    let swarm_for_send = swarm.clone();
    let creator_nick = creator.nickname.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        cli_message(&swarm_for_send, &creator_nick, "via long-poll");
    });
    let (got, resolve_elapsed) = cli_poll_wait(&swarm, &joiner.nickname, after.as_deref(), "15000");
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
        resolve_elapsed < Duration::from_secs(14),
        "resolved before the 15s timeout, took {resolve_elapsed:?}"
    );
}

/// `ahs ping` is daemon-owned: the transient command arms a round over
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

/// Join horizon: a peer that joins after history was exchanged must
/// **not surface** that pre-join history (anti-entropy still relays it
/// at the wire for swarm-wide resilience — that is intentionally not
/// observable here; only the view is filtered). A message sent *after*
/// it joined must still arrive, proving the node is meshed and only
/// the horizon, not connectivity, hides the old messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_join_horizon_hides_pre_join_history() {
    let creator = InProcNode::create("nethorizon").await;
    let mut early = InProcNode::join(&creator.swarm, "jh-early").await;

    // History exchanged *before* the late peer exists.
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
/// anti-entropy digest exchange must reconcile the gap.
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

    // `--rate-limit 0`: the burst must not be throttled on the send path.
    let (creator, swarm) = Node::create_args("itest", &["--rate-limit", "0"], &envs);
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

/// Rate-limited messages never enter the receiver's message log — which is
/// the anti-entropy recovery source — so anti-entropy can never "launder"
/// dropped spam to a peer. Drops happen on the send side (before the log
/// push, `broadcast.rs`) and the receive side (before the log push,
/// `recv.rs`); either way the excess is absent from what backfills peers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_dropped_messages_are_not_retained_for_backfill() {
    let sender = InProcNode::create("net-rl-backfill").await;
    let mut receiver = InProcNode::join(&sender.swarm, "rl-backfill-recv").await;

    let flood = RATE_LIMIT_PER_MIN as usize * 2; // twice the per-identity quota
    for index in 0..flood {
        let _ = sender.try_send(&format!("flood {index}")).await;
    }
    assert!(
        receiver.wait_inbound(1, MSG_TIMEOUT).await,
        "no flood message arrived at all"
    );
    tokio::time::sleep(Duration::from_secs(2)).await; // let gossip settle

    let retained = receiver
        .inbound()
        .iter()
        .filter(|msg| msg.body.as_str().starts_with("flood "))
        .map(|msg| msg.body.as_str().to_string())
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert!(
        retained >= 1,
        "the receiver should retain at least the burst allowance"
    );
    assert!(
        retained < flood,
        "rate limiter must keep excess out of the log (the anti-entropy source); retained all {retained}"
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

    let (creator, swarm) = Node::create_args("itest", &["--rate-limit", "0"], &envs);
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

    let (creator, swarm) = Node::create_args("itest", &["--rate-limit", "0"], &envs);
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

    let (creator, swarm) = Node::create_args("itest", &["--rate-limit", "0"], &envs);
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
    let _ = common::cli_msg_checked(&swarm, &survivor.nickname, "sv-after", None);
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
