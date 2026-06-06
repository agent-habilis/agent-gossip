//! Integration tests for the gossip network.
//!
//! Each test spawns real `ah-s` processes, exercises the network,
//! and asserts on what each node actually received. Tests are independent —
//! each creates its own swarm so IPC sockets never collide.
//!
//! Run `cargo build --release` first for faster crypto (shorter connect times).
mod common;

use std::fs::{self, File};
use std::process::Stdio;
use std::time::{Duration, Instant};

use agent_habilis_swarm::RATE_LIMIT_PER_MIN;
use common::{
    CONNECT_TIMEOUT, InProcNode, MSG_TIMEOUT, Msg, Node, POLL, SOCKET_DIR, cli_message,
    cli_message_raw, cli_ping, cli_poll, tmp_log, trace_log, wait_total, wait_until,
};

// ── tests ─────────────────────────────────────────────────────────────────────

/// Basic sanity: a broadcast message is received by the non-sending node.
/// The node whose IPC socket is used will NOT receive its own broadcast;
/// the peer will. We check total delivery across both nodes = 1.
#[tokio::test]
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

/// Sender-side rate limiting, symmetric with the receiver: a node may
/// emit up to its per-author quota, then its own sends are dropped
/// (`Ok(None)`) rather than broadcast. Mirrors the receiver-side
/// `rate_limiter_drops_excess_messages_from_flooding_peer`. One node is
/// enough — the limiter is checked before any mesh interaction.
#[tokio::test]
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
#[tokio::test]
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
#[tokio::test]
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
#[tokio::test]
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
#[tokio::test]
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
    let fake_swarm = "ahs1111111111111111111111111111111111111111111111111111111111111";
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
#[tokio::test]
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
#[tokio::test]
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
#[tokio::test]
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
#[tokio::test]
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
#[tokio::test]
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
            "ah-s-statefile-test-{}-{signal}.json",
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

        let _ = std::process::Command::new("kill")
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

/// The poll command retrieves buffered messages from a running swarm process.
/// Calling poll with --after returns only messages after that ID.
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

    // Poll all messages from joiner's process.
    let all_json = cli_poll(&swarm, &joiner.nickname, None);
    let all: Vec<serde_json::Value> = serde_json::from_str(&all_json)
        .unwrap_or_else(|error| panic!("failed to parse poll JSON: {error}\nraw: {all_json}"));

    // Should have at least the 2 messages we sent (plus possible presence messages).
    let msg_bodies: Vec<&str> = all.iter().filter_map(|msg| msg["body"].as_str()).collect();
    assert!(
        msg_bodies.contains(&"hello from poll test"),
        "first message missing from poll: {msg_bodies:?}"
    );
    assert!(
        msg_bodies.contains(&"second message"),
        "second message missing from poll: {msg_bodies:?}"
    );

    // Find the ID of the first sent message and poll with --after.
    let first_msg = all
        .iter()
        .find(|msg| msg["body"].as_str() == Some("hello from poll test"))
        .expect("first message not found");
    let first_id = first_msg["id"].as_str().expect("message has no id");

    let after_json = cli_poll(&swarm, &joiner.nickname, Some(first_id));
    let after: Vec<serde_json::Value> = serde_json::from_str(&after_json).unwrap_or_else(|error| {
        panic!("failed to parse after-poll JSON: {error}\nraw: {after_json}")
    });

    // Should NOT contain the first message, but should contain the second.
    let after_bodies: Vec<&str> = after
        .iter()
        .filter_map(|msg| msg["body"].as_str())
        .collect();
    assert!(
        !after_bodies.contains(&"hello from poll test"),
        "--after should exclude the referenced message"
    );
    assert!(
        after_bodies.contains(&"second message"),
        "second message missing from --after poll: {after_bodies:?}"
    );
}

/// `ah-s ping` is daemon-owned: the transient command arms a round over
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
    // Allow detection of the departure + one heal tick for the
    // bystander to win the freed port and stand its rendezvous up.
    std::thread::sleep(Duration::from_secs(22));

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
    // One heal tick for the bystander to claim the freed rendezvous.
    std::thread::sleep(Duration::from_secs(22));

    let joiner = Node::join(&swarm, "fm-joiner");
    assert!(
        joiner.wait_ready(&swarm),
        "joiner could not join after creator death\nbystander:\n{}\njoiner:\n{}",
        bystander.log_tail(15),
        joiner.log_tail(15),
    );

    // First broadcast, sent immediately after `ready` (the exact bug
    // trigger): joiner -> bystander.
    let j2b_id = cli_message(&swarm, &joiner.nickname, "j2b first");
    assert!(!j2b_id.is_empty(), "joiner msg returned empty id");
    assert!(
        wait_until(
            || bystander.count_from(&joiner.nickname, "j2b first"),
            1,
            MSG_TIMEOUT
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
            MSG_TIMEOUT
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
#[tokio::test]
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
    // >= 2 * HEAL_INTERVAL_SECS (15s) + margin for claim-if-free and a
    // cold joiner's connect. Irreducible (heal cadence is a `const`).
    let handoff = Duration::from_secs(36);

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
    assert_received(&bravo, &alpha.nickname, "ck-survive", MSG_TIMEOUT);

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
    assert_received(&alpha, &charlie.nickname, "ck-newcomer", MSG_TIMEOUT);
}

/// Sleep/wake: `SIGSTOP` a peer past the (shortened) alive-timeout so
/// the swarm evicts it, then `SIGCONT` and assert the heal primitive
/// re-meshes it and traffic resumes.
#[test]
fn test_sleep_wake_heal_recovery() {
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
    assert_received(
        &sleeper,
        &creator.nickname,
        "sw-post",
        Duration::from_mins(1),
    );
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
    assert_received(
        &sleeper,
        &creator.nickname,
        "rb-post",
        Duration::from_mins(1),
    );
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
#[tokio::test]
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
#[tokio::test]
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
#[tokio::test]
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
#[tokio::test]
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
