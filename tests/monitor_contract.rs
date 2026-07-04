//! Integration tests for the Monitor tool contract.
//!
//! The Claude Code Monitor tool runs a command in the background and feeds each
//! stdout line back to Claude as an event. These tests validate that the daemon's
//! JSON stdout output matches the event shapes the skill's Monitor event handler
//! expects.
//!
//! Uses `--output json` mode (the mode Monitor will use) and 3 peers to test
//! multi-peer dynamics.
//!
//! Run `cargo build --release` first for faster crypto (shorter connect times).
mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use agent_gossip::Channel;
use common::{
    CONNECT_TIMEOUT, InProcNode, MSG_TIMEOUT, POLL, RECOVERY_TIMEOUT, serial_guard, socket_path,
    tmp_log, wait_until,
};

/// A node running in JSON output mode, with stdout captured to a log file.
struct JsonNode {
    child: Child,
    log: PathBuf,
    pub nickname: String,
}

impl JsonNode {
    /// Spawn `agent-gossip create --no-interactive --output json`, wait for the
    /// `ready` event, and return the node + swarm identifier.
    fn create() -> (Self, String) {
        Self::create_with_flags(&[])
    }

    /// Like [`create`](Self::create) but passes extra hidden tuning flags to
    /// the spawned daemon as `(flag, value)` pairs (e.g. shortened eviction
    /// timings).
    fn create_with_flags(flags: &[(&str, &str)]) -> (Self, String) {
        let log = tmp_log("json-create");
        let file = fs::File::create(&log).unwrap();
        let child = common::test_cmd()
            .args([
                "create",
                "--name",
                "jtest",
                "--no-interactive",
                "--output",
                "json",
            ])
            .args(common::flag_args(flags))
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .spawn()
            .expect("failed to spawn create");

        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut swarm = None;
        let mut name = None;
        let mut nickname = None;

        while Instant::now() < deadline && (swarm.is_none() || nickname.is_none() || name.is_none())
        {
            let content = fs::read_to_string(&log).unwrap_or_default();
            for line in content.lines() {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line)
                    && parsed["event"] == "ready"
                {
                    swarm = parsed["swarm"].as_str().map(ToString::to_string);
                    name = parsed["name"].as_str().map(ToString::to_string);
                    nickname = parsed["nickname"].as_str().map(ToString::to_string);
                }
            }
            if swarm.is_none() || nickname.is_none() || name.is_none() {
                std::thread::sleep(POLL);
            }
        }

        let swarm_id = swarm.expect("timed out waiting for ready event with swarm");
        let name = name.expect("timed out waiting for ready event with name");
        let nick = nickname.expect("timed out waiting for ready event with nickname");
        assert_eq!(name, "jtest", "ready event must surface the create --name");
        (
            JsonNode {
                child,
                log,
                nickname: nick,
            },
            swarm_id,
        )
    }

    /// Spawn `agent-gossip join <swarm> --nickname <nickname> --no-interactive --output json`.
    fn join(swarm: &str, nickname: &str) -> Self {
        Self::join_with_flags(swarm, nickname, &[])
    }

    /// Like [`join`](Self::join) but passes extra hidden tuning flags to the
    /// spawned daemon as `(flag, value)` pairs.
    fn join_with_flags(swarm: &str, nickname: &str, flags: &[(&str, &str)]) -> Self {
        let log = tmp_log(&format!("json-{nickname}"));
        let file = fs::File::create(&log).unwrap();
        let child = common::test_cmd()
            .args([
                "join",
                swarm,
                "--nickname",
                nickname,
                "--no-interactive",
                "--output",
                "json",
            ])
            .args(common::flag_args(flags))
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .spawn()
            .expect("failed to spawn join");
        JsonNode {
            child,
            log,
            nickname: nickname.to_string(),
        }
    }

    fn wait_ready(&self, swarm: &str) -> bool {
        let sock = socket_path(swarm, &self.nickname);
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            if std::path::Path::new(&sock).exists() {
                return true;
            }
            std::thread::sleep(POLL);
        }
        eprintln!(
            "wait_ready timed out for {} (looking for {})",
            self.nickname, sock
        );
        false
    }

    /// Read all JSON lines from the log file.
    fn json_events(&self) -> Vec<serde_json::Value> {
        let content = fs::read_to_string(&self.log).unwrap_or_default();
        content
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect()
    }

    /// Filter events to only `{"event":"message"}` entries.
    fn message_events(&self) -> Vec<serde_json::Value> {
        self.json_events()
            .into_iter()
            .filter(|value| value["event"] == "message")
            .collect()
    }

    /// Filter to message events that are actual messages (type=msg), not presence.
    fn msg_events(&self) -> Vec<serde_json::Value> {
        self.message_events()
            .into_iter()
            .filter(|value| value["type"] == "msg")
            .collect()
    }

    /// Filter to presence events.
    fn presence_events(&self) -> Vec<serde_json::Value> {
        self.message_events()
            .into_iter()
            .filter(|value| value["type"] == "presence")
            .collect()
    }

    fn sigint(&self) {
        let _ = Command::new("kill")
            .args(["-2", &self.child.id().to_string()])
            .status();
    }

    /// Check if the process is still running (has not exited).
    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for JsonNode {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.log);
    }
}

// ── CLI helpers ─────────────────────────────────────────────────────────────

// Thin shims over `common::cli_*`. Monitor tests send via IPC to an
// already-running daemon; success is asserted.

fn cli_send(swarm: &str, nickname: &str, body: &str) -> String {
    common::cli_msg_checked(swarm, nickname, body)
}

// ── test fixtures ──────────────────────────────────────────────────────────

/// Spawn a 3-peer swarm (1 creator + 2 joiners) and wait for overlay to stabilise.
fn three_peers(suffix: &str) -> (JsonNode, JsonNode, JsonNode, String) {
    let (creator, swarm) = JsonNode::create();
    let joiner_a = JsonNode::join(&swarm, &format!("mon-{suffix}-a"));
    let joiner_b = JsonNode::join(&swarm, &format!("mon-{suffix}-b"));

    assert!(creator.wait_ready(&swarm));
    assert!(joiner_a.wait_ready(&swarm));
    assert!(joiner_b.wait_ready(&swarm));

    std::thread::sleep(Duration::from_secs(3));

    (creator, joiner_a, joiner_b, swarm)
}

// ── tests ───────────────────────────────────────────────────────────────────

/// The `ready` event has the expected JSON shape with `swarm`, `name`,
/// and `nickname`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_ready_event_shape() {
    let mut creator = InProcNode::create("readyshape").await;
    let swarm = creator.swarm.clone();
    let nick = creator.nickname.clone();
    assert!(swarm.starts_with("💬"));
    assert!(!nick.is_empty());

    let events = creator.json_events();
    let ready = events
        .iter()
        .find(|value| value["event"] == "ready")
        .expect("no ready event found");

    assert_eq!(ready["event"], "ready");
    assert!(ready["swarm"].is_string());
    assert!(ready["name"].is_string());
    assert!(ready["nickname"].is_string());
    assert_eq!(ready["swarm"].as_str().unwrap(), swarm);
    assert_eq!(ready["name"].as_str().unwrap(), "readyshape");
    assert_eq!(ready["nickname"].as_str().unwrap(), nick);
    // The build self-identifies: the ready event carries the exact version
    // string (crate version + git sha + dirty flag).
    assert_eq!(ready["version"].as_str().unwrap(), agent_gossip::VERSION);
}

/// Three peers: creator surfaces a membership `joined` presence for
/// both joiners. Arrival is surfaced once, via nickname-keyed
/// presence (not a transport-level event).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_peer_discovery_three_peers() {
    let (mut creator, _joiner_a, _joiner_b) = common::three_peers("disco").await;

    assert!(
        creator.wait_presence_count(true, 2, MSG_TIMEOUT).await,
        "expected >=2 joined presence events"
    );
}

/// Message from joiner-A is received by creator AND joiner-B with correct JSON shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_cross_peer_message_delivery() {
    let (mut creator, joiner_a, mut joiner_b) = common::three_peers("xpeer").await;

    joiner_a.send("hello from A").await;

    assert!(
        creator.wait_inbound(1, MSG_TIMEOUT).await,
        "creator never received joiner_a's message"
    );
    assert!(
        joiner_b.wait_inbound(1, MSG_TIMEOUT).await,
        "joiner_b never received joiner_a's message"
    );

    let creator_msgs = creator.msg_events();
    let joiner_b_msgs = joiner_b.msg_events();
    let msg = creator_msgs
        .iter()
        .chain(joiner_b_msgs.iter())
        .find(|value| value["body"] == "hello from A")
        .expect("no message with body 'hello from A' found");

    assert_eq!(msg["event"], "message");
    assert_eq!(msg["type"], "msg");
    assert!(msg["id"].is_string(), "missing id field");
    assert_eq!(msg["author"], "mon-xpeer-a");
    assert!(msg["ts"].is_number(), "ts should be a number");
    assert_eq!(msg["body"], "hello from A");
    assert!(msg["to"].is_null(), "message should have to: null");
    assert_eq!(
        msg["message"]["role"], "ROLE_USER",
        "the embedded a2a payload rides the event"
    );
    assert_eq!(msg["self"], false, "receiver should see self: false");
}

/// Messages from both joiners are received by the other peers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bidirectional_multi_peer() {
    let (mut creator, joiner_a, joiner_b) = common::three_peers("bidir").await;

    joiner_a.send("msg from A").await;
    joiner_b.send("msg from B").await;

    assert!(
        creator.wait_inbound(2, MSG_TIMEOUT).await,
        "creator expected >=2 messages"
    );

    let bodies: Vec<String> = creator
        .msg_events()
        .iter()
        .filter_map(|value| value["body"].as_str().map(str::to_owned))
        .collect();
    assert!(bodies.iter().any(|body| body == "msg from A"));
    assert!(bodies.iter().any(|body| body == "msg from B"));
}

/// Self-echo suppression: the sender does NOT see its own message in JSON output
/// (the event loop skips messages where author == self). Messages sent via IPC
/// are echoed back with `"self":true`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_self_echo_suppression() {
    let mut creator = InProcNode::create("monecho").await;
    let mut joiner = InProcNode::join(&creator.swarm, "mon-echo").await;

    joiner.send("echo test").await;

    assert!(
        creator.wait_inbound(1, MSG_TIMEOUT).await,
        "creator did not receive the message"
    );
    assert!(
        joiner.wait_messages(1, MSG_TIMEOUT).await,
        "joiner did not see its own IPC echo"
    );

    let creator_msgs = creator.msg_events();
    let message = creator_msgs
        .iter()
        .find(|entry| entry["body"] == "echo test")
        .expect("creator did not receive message");
    assert_eq!(message["self"], false);

    let joiner_msgs = joiner.msg_events();
    assert!(
        joiner_msgs
            .iter()
            .any(|value| value["body"] == "echo test" && value["self"] == true),
        "joiner should see its own message echoed with self:true"
    );
    // Gossip-level echo is suppressed by the event loop (author == self).
    assert!(
        !joiner_msgs
            .iter()
            .any(|value| value["body"] == "echo test" && value["self"] == false),
        "joiner should NOT see its own message via gossip (self:false)"
    );
}

/// Peer departure: when joiner-A is killed via SIGINT, creator receives a
/// presence `left` event with the correct JSON shape.
#[test]
fn test_peer_departure_event() {
    // Disruption/recovery test — serialize against the other heavy departure
    // tests so concurrent SIGINTs don't starve each other's heal cycles (the
    // same gate gossip_network's reliability tests use).
    let _serial = serial_guard();
    let (creator, joiner_a, _joiner_b, _swarm) = three_peers("depart");

    joiner_a.sigint();

    wait_until(
        || {
            creator
                .presence_events()
                .iter()
                .filter(|value| value["subtype"] == "left")
                .count()
        },
        1,
        MSG_TIMEOUT,
    );

    let left = creator
        .presence_events()
        .into_iter()
        .find(|value| value["subtype"] == "left")
        .expect("creator did not receive left event");

    assert_eq!(left["event"], "message");
    assert_eq!(left["type"], "presence");
    assert_eq!(left["subtype"], "left");
    assert!(left["author"].is_string(), "presence event missing author");
    assert!(left["ts"].is_number(), "presence event missing ts");
    assert!(left["id"].is_string(), "presence event missing id");
}

/// Presence `joined` events have the correct JSON shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_presence_joined_event_shape() {
    let mut creator = InProcNode::create("monjshape").await;
    let _joiner = InProcNode::join(&creator.swarm, "mon-joined-shape").await;

    assert!(
        creator
            .wait_presence("mon-joined-shape", true, MSG_TIMEOUT)
            .await,
        "creator did not receive joined presence from mon-joined-shape"
    );

    let joined_event = creator
        .json_events()
        .into_iter()
        .find(|value| value["subtype"] == "joined" && value["author"] == "mon-joined-shape")
        .expect("creator did not receive joined presence from mon-joined-shape");

    assert_eq!(joined_event["event"], "message");
    assert_eq!(joined_event["type"], "presence");
    assert_eq!(joined_event["subtype"], "joined");
    assert!(joined_event["author"].is_string());
    assert!(joined_event["ts"].is_number());
    assert!(joined_event["id"].is_string());
    assert_eq!(joined_event["author"], "mon-joined-shape");
}

/// Info events have the expected shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_info_event_shape() {
    let mut creator = InProcNode::create("moninfo").await;

    let info_events: Vec<_> = creator
        .json_events()
        .into_iter()
        .filter(|value| value["event"] == "info")
        .collect();

    assert!(
        !info_events.is_empty(),
        "expected at least one info event from daemon startup"
    );

    let info = &info_events[0];
    assert_eq!(info["event"], "info");
    assert!(
        info["message"].is_string(),
        "info event missing message field"
    );
}

/// Every JSON line from the daemon is valid JSON (no partial lines, no corruption).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_all_lines_are_valid_json() {
    let mut creator = InProcNode::create("monvalid").await;
    let joiner = InProcNode::join(&creator.swarm, "mon-valid-json").await;

    joiner.send("json validity test").await;
    assert!(
        creator.wait_inbound(1, MSG_TIMEOUT).await,
        "message never arrived"
    );

    // Every event the daemon would write in `--output json` mode must
    // render to valid JSON (SwarmId is the bare stderr line → None).
    for event in creator.events() {
        if let Some(line) = agent_gossip::event_json(event) {
            assert!(
                serde_json::from_str::<serde_json::Value>(&line).is_ok(),
                "rendered line is not valid JSON: {line:?}"
            );
        }
    }
}

/// Every message event has all fields the Monitor event handler needs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_message_event_has_all_required_fields() {
    let mut creator = InProcNode::create("monfields").await;
    let joiner = InProcNode::join(&creator.swarm, "mon-fields").await;

    joiner.send("field check").await;
    assert!(
        creator.wait_inbound(1, MSG_TIMEOUT).await,
        "message never arrived"
    );

    let msgs = creator.msg_events();
    for msg in &msgs {
        assert!(msg["event"].is_string(), "missing event field: {msg}");
        assert!(msg["id"].is_string(), "missing id field: {msg}");
        assert!(msg["type"].is_string(), "missing type field: {msg}");
        assert!(msg["author"].is_string(), "missing author field: {msg}");
        assert!(msg["ts"].is_number(), "missing ts field: {msg}");
        assert!(msg["body"].is_string(), "missing body field: {msg}");
        // to should be present (null or string).
        assert!(
            msg["to"].is_null() || msg["to"].is_string(),
            "to should be null or string: {msg}"
        );
        // self should be a boolean.
        assert!(msg["self"].is_boolean(), "missing self field: {msg}");
    }
}

/// When the creator leaves, remaining peers should stay alive and be able
/// to pass messages.
#[test]
fn test_creator_departure_peers_survive() {
    // Disruption/recovery test — serialize so concurrent SIGINTs don't starve
    // each other's heal cycles (see `serial_guard`).
    let _serial = serial_guard();
    let (creator, swarm) = JsonNode::create();
    let mut joiner_a = JsonNode::join(&swarm, "survive-alpha");
    let mut joiner_b = JsonNode::join(&swarm, "survive-beta");

    assert!(creator.wait_ready(&swarm));
    assert!(joiner_a.wait_ready(&swarm));
    assert!(joiner_b.wait_ready(&swarm));

    // Wait for gossip overlay to stabilise.
    let count = wait_until(
        || {
            creator
                .presence_events()
                .iter()
                .filter(|value| value["subtype"] == "joined")
                .count()
        },
        2,
        MSG_TIMEOUT,
    );
    assert!(count >= 2, "creator did not see both joiners");

    // Kill the creator.
    creator.sigint();
    std::thread::sleep(Duration::from_secs(3));

    // Both joiners should still be alive.
    assert!(joiner_a.is_alive(), "joiner_a died after creator departure");
    assert!(joiner_b.is_alive(), "joiner_b died after creator departure");

    // Joiners should still be able to pass messages. This is a
    // post-disruption delivery: the creator may have been the relay hub
    // between the two joiners, so re-meshing waits on the heal cadence —
    // budget it with `RECOVERY_TIMEOUT`, not the steady-state `MSG_TIMEOUT`.
    cli_send(&swarm, "survive-alpha", "still-here");
    let delivered = wait_until(|| joiner_b.msg_events().len(), 1, RECOVERY_TIMEOUT);
    assert!(
        delivered >= 1,
        "joiner_b did not receive message from joiner_a after creator left"
    );

    let msg = &joiner_b.msg_events()[0];
    assert_eq!(msg["author"], "survive-alpha");
    assert_eq!(msg["body"], "still-here");
}

/// When the creator is hard-killed, joiners should stay alive.
#[test]
fn test_creator_hard_kill_peers_survive() {
    let (mut creator, swarm) = JsonNode::create();
    let mut joiner_a = JsonNode::join(&swarm, "hk-alpha");

    assert!(creator.wait_ready(&swarm));
    assert!(joiner_a.wait_ready(&swarm));

    let count = wait_until(
        || {
            creator
                .presence_events()
                .iter()
                .filter(|value| value["subtype"] == "joined")
                .count()
        },
        1,
        MSG_TIMEOUT,
    );
    assert!(count >= 1, "creator did not see joiner");

    // Hard kill — no graceful `left` message.
    let _ = creator.child.kill();
    let _ = creator.child.wait();
    std::thread::sleep(Duration::from_secs(3));

    assert!(
        joiner_a.is_alive(),
        "joiner_a should stay alive after creator was hard-killed"
    );
}

/// With 4 peers, all joiners survive after creator departs and can
/// continue exchanging messages.
#[test]
fn test_creator_departure_four_peers_survive() {
    // Disruption/recovery test — serialize so concurrent SIGINTs don't starve
    // each other's heal cycles (see `serial_guard`).
    let _serial = serial_guard();
    let (creator, swarm) = JsonNode::create();
    let mut joiner_a = JsonNode::join(&swarm, "four-alpha");
    let mut joiner_b = JsonNode::join(&swarm, "four-beta");
    let mut joiner_c = JsonNode::join(&swarm, "four-gamma");

    assert!(creator.wait_ready(&swarm));
    assert!(joiner_a.wait_ready(&swarm));
    assert!(joiner_b.wait_ready(&swarm));
    assert!(joiner_c.wait_ready(&swarm));

    let count = wait_until(
        || {
            creator
                .presence_events()
                .iter()
                .filter(|value| value["subtype"] == "joined")
                .count()
        },
        3,
        MSG_TIMEOUT,
    );
    assert!(count >= 3, "creator did not see all 3 joiners");

    // Verify cross-joiner messaging works before killing creator.
    cli_send(&swarm, "four-alpha", "mesh-check");
    let mesh_count = wait_until(
        || {
            joiner_c
                .msg_events()
                .iter()
                .filter(|value| value["body"] == "mesh-check")
                .count()
        },
        1,
        MSG_TIMEOUT,
    );
    assert!(
        mesh_count >= 1,
        "mesh not formed: joiner_c did not receive from joiner_a"
    );

    // Kill the creator.
    creator.sigint();
    std::thread::sleep(Duration::from_secs(3));

    // All 3 joiners should still be alive.
    assert!(joiner_a.is_alive(), "joiner_a died after creator departure");
    assert!(joiner_b.is_alive(), "joiner_b died after creator departure");
    assert!(joiner_c.is_alive(), "joiner_c died after creator departure");

    // Verify cross-peer messaging still works. Post-disruption deliveries
    // (the creator just left) re-mesh on the heal cadence, so they get
    // `RECOVERY_TIMEOUT` rather than the steady-state `MSG_TIMEOUT`.
    cli_send(&swarm, "four-alpha", "post-creator-msg");
    let post_count = wait_until(|| joiner_b.msg_events().len(), 1, RECOVERY_TIMEOUT);
    assert!(
        post_count >= 1,
        "joiner_b did not receive message after creator left"
    );

    cli_send(&swarm, "four-beta", "post-creator-relay");
    let relay_count = wait_until(
        || {
            joiner_c
                .msg_events()
                .iter()
                .filter(|value| value["body"] == "post-creator-relay")
                .count()
        },
        1,
        RECOVERY_TIMEOUT,
    );
    assert!(
        relay_count >= 1,
        "joiner_c did not receive message from joiner_b after creator left"
    );
}

// ── heartbeat / peer_timeout tests ──────────────────────────────────────────

/// A heartbeat `Presence::Alive` never appears as a `message` event in
/// the JSON output — it's an internal keepalive. Fast check: no timing
/// dependency beyond a brief settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_alive_presence_is_silent_in_json() {
    let mut creator = InProcNode::create("monsilent").await;
    let mut joiner = InProcNode::join(&creator.swarm, "silent-alpha").await;

    // Let connection + any early `joined` chatter + alive ticks settle.
    tokio::time::sleep(Duration::from_secs(3)).await;

    for node in [&mut creator, &mut joiner] {
        for event in node.json_events() {
            assert_ne!(
                event["subtype"], "alive",
                "Alive presence leaked to JSON output: {event}"
            );
        }
    }
}

/// When a peer is hard-killed, the survivor eventually emits a
/// `peer_timeout` event and their state file drops to 1.
///
/// Shortens the eviction window by passing `ALIVE_TIMEOUT_SECS=3` and
/// `SWEEP_INTERVAL_SECS=1` to the survivor daemon. The env is scoped to
/// that child process, so nothing leaks to sibling tests in the binary.
#[test]
fn test_hard_kill_triggers_peer_timeout() {
    let state_file = tmp_log("state-survivor");
    let log = tmp_log("json-survivor");
    let file = fs::File::create(&log).unwrap();
    let mut creator = common::test_cmd()
        .args([
            "create",
            "--name",
            "hardkill",
            "--no-interactive",
            "--output",
            "json",
            "--state-file",
            state_file.to_str().unwrap(),
        ])
        .args(["--alive-timeout-secs", "3", "--sweep-interval-secs", "1"])
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file))
        .spawn()
        .expect("failed to spawn creator");

    // Wait for creator ready.
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut swarm = None;
    while Instant::now() < deadline && swarm.is_none() {
        let content = fs::read_to_string(&log).unwrap_or_default();
        for line in content.lines() {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line)
                && parsed["event"] == "ready"
            {
                swarm = parsed["swarm"].as_str().map(ToString::to_string);
            }
        }
        std::thread::sleep(POLL);
    }
    let swarm = swarm.expect("creator never emitted ready");

    // Spawn a joiner we can kill.
    let mut victim = JsonNode::join(&swarm, "victim-alpha");
    assert!(victim.wait_ready(&swarm));

    // Confirm survivor sees participant_count = 2 in state file.
    let count_deadline = Instant::now() + MSG_TIMEOUT;
    let mut got_two = false;
    while Instant::now() < count_deadline {
        if let Ok(raw) = fs::read_to_string(&state_file)
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw)
            && parsed["participant_count"] == 2
        {
            got_two = true;
            break;
        }
        std::thread::sleep(POLL);
    }
    assert!(got_two, "state file never reached participant_count=2");

    // Hard-kill the victim.
    let _ = victim.child.kill();
    let _ = victim.child.wait();

    // With env-overridden timeouts (3s + 1s) the eviction should land
    // well under 10s; give 15s for CI jitter.
    let eviction_window = Duration::from_secs(15);
    let eviction_deadline = Instant::now() + eviction_window;
    let mut evicted = false;
    while Instant::now() < eviction_deadline {
        if let Ok(raw) = fs::read_to_string(&state_file)
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw)
            && parsed["participant_count"] == 1
        {
            evicted = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    assert!(
        evicted,
        "state file never dropped to participant_count=1 after hard kill"
    );

    // Confirm the JSON stream emitted exactly one peer_timeout event.
    let content = fs::read_to_string(&log).unwrap_or_default();
    let timeouts: Vec<&str> = content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| value["event"] == "peer_timeout")
        .filter_map(|value| value["nickname"].as_str().map(ToString::to_string))
        .map(|_| "ok")
        .collect();
    assert_eq!(
        timeouts.len(),
        1,
        "expected exactly one peer_timeout event, got {}: {}",
        timeouts.len(),
        content
    );

    // Cleanup.
    let _ = creator.kill();
    let _ = creator.wait();
    let _ = fs::remove_file(&log);
    let _ = fs::remove_file(&state_file);
}

/// Join-horizon symmetry: a peer that joined AND departed before this
/// process joined — and is therefore known only through relayed
/// pre-join anti-entropy backlog — must never surface as `has joined`,
/// `peer_timeout` ("went quiet"), or a message. A peer still alive at
/// join time must, by contrast, surface exactly one `has joined`.
///
/// Shortens the eviction window via `ALIVE_TIMEOUT_SECS=3` /
/// `SWEEP_INTERVAL_SECS=1` (scoped to each spawned daemon) so the ghost
/// would be swept (and, pre-fix, emit a bogus `peer_timeout`) well
/// within the test window.
#[test]
fn test_pre_join_ghost_peer_never_surfaces() {
    let short_evict = [
        ("--alive-timeout-secs", "3"),
        ("--sweep-interval-secs", "1"),
    ];

    let (creator, swarm) = JsonNode::create_with_flags(&short_evict);
    assert!(creator.wait_ready(&swarm), "creator never ready");

    // A peer joins, talks, then is hard-killed — all before the late
    // joiner exists. The creator logs its `joined` + `msg`, so
    // anti-entropy will later relay both to the late joiner.
    let mut ghost = JsonNode::join(&swarm, "jh-ghost");
    assert!(ghost.wait_ready(&swarm), "ghost never ready");
    let _ = cli_send(&swarm, &ghost.nickname, "ghost-hist");
    assert!(
        wait_until(
            || creator
                .msg_events()
                .iter()
                .filter(|value| value["author"] == "jh-ghost" && value["body"] == "ghost-hist")
                .count(),
            1,
            MSG_TIMEOUT,
        ) >= 1,
        "creator never logged the ghost's history (anti-entropy source missing)"
    );
    let _ = ghost.child.kill();
    let _ = ghost.child.wait();
    drop(ghost);

    // Whole-second timestamps: keep the ghost's history strictly an
    // earlier second than the late join (off the 1s boundary).
    std::thread::sleep(Duration::from_secs(2));

    let late = JsonNode::join_with_flags(&swarm, "jh-late", &short_evict);
    assert!(late.wait_ready(&swarm), "late joiner never ready");

    // Positive control: the still-present creator must surface, proving
    // the late joiner is meshed and the horizon — not connectivity — is
    // what hides the ghost.
    let _ = cli_send(&swarm, &creator.nickname, "live-after");
    assert!(
        wait_until(
            || late
                .msg_events()
                .iter()
                .filter(|value| value["author"] == creator.nickname.as_str()
                    && value["body"] == "live-after")
                .count(),
            1,
            MSG_TIMEOUT,
        ) >= 1,
        "post-join message from the live creator never reached the late joiner\n{:?}",
        late.json_events()
    );

    // Well over an anti-entropy cycle + several (1s) sweep cycles past
    // the (3s) timeout: any ghost surfacing would have happened by now.
    std::thread::sleep(Duration::from_secs(20));

    let ghost_joined = late
        .presence_events()
        .into_iter()
        .filter(|value| value["subtype"] == "joined" && value["author"] == "jh-ghost")
        .count();
    let ghost_timeouts = late
        .json_events()
        .into_iter()
        .filter(|value| value["event"] == "peer_timeout" && value["nickname"] == "jh-ghost")
        .count();
    let ghost_msgs = late
        .msg_events()
        .into_iter()
        .filter(|value| value["author"] == "jh-ghost")
        .count();
    let creator_joined = late
        .presence_events()
        .into_iter()
        .filter(|value| {
            value["subtype"] == "joined" && value["author"] == creator.nickname.as_str()
        })
        .count();

    assert_eq!(
        ghost_joined,
        0,
        "pre-join ghost surfaced a `has joined`\n{:?}",
        late.json_events()
    );
    assert_eq!(
        ghost_timeouts,
        0,
        "pre-join ghost surfaced a `peer_timeout` (went quiet)\n{:?}",
        late.json_events()
    );
    assert_eq!(
        ghost_msgs,
        0,
        "pre-join ghost history was surfaced as a message\n{:?}",
        late.json_events()
    );
    assert_eq!(
        creator_joined,
        1,
        "still-present creator must surface exactly one `has joined`\n{:?}",
        late.json_events()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_with_nickname_uses_custom_name() {
    let node = InProcNode::create_with_nick("nickjoin", "pinned-manager").await;
    assert_eq!(
        node.nickname, "pinned-manager",
        "expected nickname 'pinned-manager', got '{}'",
        node.nickname
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_without_nickname_uses_random_name() {
    let node = InProcNode::create("randnick").await;
    let nick = &node.nickname;
    assert!(
        !nick.is_empty(),
        "nickname should not be empty when none is provided"
    );
    // word-word pattern: two lowercase words separated by a hyphen.
    let parts: Vec<&str> = nick.split('-').collect();
    assert_eq!(
        parts.len(),
        2,
        "nickname '{nick}' should be in word-word format"
    );
    assert!(
        parts[0].chars().all(|ch| ch.is_ascii_lowercase()),
        "first word '{}' should be lowercase ASCII",
        parts[0]
    );
    assert!(
        parts[1].chars().all(|ch| ch.is_ascii_lowercase()),
        "second word '{}' should be lowercase ASCII",
        parts[1]
    );
}

// ── task + peers wire contract ───────────────────────────────────────────────

/// The `task` event wire shape: creating a task (a directed `message/send`)
/// is surfaced on **the worker's** `--output json` stream as a `message`-kind
/// task event with the documented fields, and is **absent** from a third
/// peer's stream (delivered for relay, never surfaced to a bystander).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_task_event_wire_contract() {
    let (creator, joiner_a, joiner_b, swarm) = three_peers("ho");
    let worker = joiner_a.nickname.clone();

    let task_id = common::cli_task_create(&swarm, &creator.nickname, &worker, "## Task\nport it");

    // The worker surfaces the incoming `message/send` as a task event.
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut task = None;
    while Instant::now() < deadline {
        if let Some(event) = joiner_a
            .json_events()
            .into_iter()
            .find(|value| value["event"] == "task")
        {
            task = Some(event);
            break;
        }
        std::thread::sleep(POLL);
    }
    let task = task.expect("the worker never surfaced a task event");
    assert_eq!(task["event"], "task");
    assert_eq!(task["kind"], "message");
    assert_eq!(task["task_id"], task_id);
    assert_eq!(task["to"], worker);
    assert_eq!(task["author"], creator.nickname);
    assert_eq!(task["body"], "## Task\nport it");
    assert_eq!(task["self"], false);
    assert!(task["id"].is_string());
    assert!(task["ts"].is_number());
    assert!(
        task.get("phase").is_none(),
        "no phase field in the native model"
    );
    // A distinct top-level event — never the `message` family.
    assert!(task.get("type").is_none());

    // The bystander relayed it but must never surface it.
    assert!(
        joiner_b
            .json_events()
            .iter()
            .all(|value| value["event"] != "task"),
        "a third peer must not surface a task addressed to someone else"
    );
}

/// A directed `message/send` (task creation) to a nickname that is not a
/// current participant exits non-zero with an `unknown participant` error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_task_unknown_participant_errors() {
    let (creator, _joiner_a, _joiner_b, swarm) = three_peers("ho-unknown");

    let out = common::cli_task_create_raw(&swarm, &creator.nickname, "ghost-peer", "brief");
    assert!(
        !out.status.success(),
        "task creation to an unknown participant must exit non-zero"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("unknown participant"),
        "expected an unknown-participant error, got: {combined}"
    );
}

/// Task timers: while both peers are alive the ball-owner's daemon
/// keepalive prevents a spurious idle-timeout; once the ball-owner dies,
/// the other party's debounce fires a `task_timeout`. Run with shortened
/// timers (`--task-timeout-secs 3`, keepalive 1s, sweep 1s).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_task_idle_timeout_after_owner_dies() {
    let timers: &[(&str, &str)] = &[
        ("--task-timeout-secs", "3"),
        ("--task-keepalive-secs", "1"),
        ("--sweep-interval-secs", "1"),
    ];
    let (creator, swarm) = JsonNode::create_with_flags(timers);
    let mut joiner = JsonNode::join_with_flags(&swarm, "tk-bob", timers);
    assert!(creator.wait_ready(&swarm));
    assert!(joiner.wait_ready(&swarm));

    // The creator must see the joiner before it can offer.
    let saw_join = wait_until(
        || {
            creator
                .presence_events()
                .iter()
                .filter(|value| value["subtype"] == "joined")
                .count()
        },
        1,
        MSG_TIMEOUT,
    );
    assert!(saw_join >= 1, "creator never saw the joiner join");

    // Creator creates the task; the worker (joiner) holds the ball and accepts.
    let tid = common::cli_task_create(&swarm, &creator.nickname, "tk-bob", "port it");
    let saw_offer = wait_until(
        || {
            joiner
                .json_events()
                .iter()
                .filter(|value| value["event"] == "task" && value["kind"] == "message")
                .count()
        },
        1,
        MSG_TIMEOUT,
    );
    assert!(saw_offer >= 1, "joiner never surfaced the task message");
    common::cli_task_status(&swarm, "tk-bob", &tid, "working");

    // Both alive well past the 3s timeout: the joiner's keepalive must keep
    // the task alive — no spurious `task_timeout` on the creator.
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        creator
            .json_events()
            .iter()
            .all(|value| value["event"] != "task_timeout"),
        "keepalive should prevent a spurious task_timeout while the owner is alive"
    );

    // Kill the ball-owner. Its keepalive stops; the creator's debounce fires.
    let _ = joiner.child.kill();
    let timed_out = wait_until(
        || {
            creator
                .json_events()
                .iter()
                .filter(|value| {
                    value["event"] == "task_timeout" && value["task_id"] == tid.as_str()
                })
                .count()
        },
        1,
        RECOVERY_TIMEOUT,
    );
    assert!(
        timed_out >= 1,
        "creator should emit task_timeout after the ball-owner died"
    );
}

/// The keepalive is bounded by skill liveness, not process liveness: a
/// ball-owner whose daemon stays **alive** but whose *skill* goes silent past
/// `--task-keepalive-max-secs` stops being covered, so the peer's debounce
/// fires a `task_timeout` — a crashed/abandoned skill can't hold the peer
/// forever. Shortened timers make the window seconds, not minutes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_task_times_out_when_skill_goes_silent() {
    let timers: &[(&str, &str)] = &[
        ("--task-timeout-secs", "3"),
        ("--task-keepalive-secs", "1"),
        ("--task-keepalive-max-secs", "2"),
        ("--sweep-interval-secs", "1"),
    ];
    let (creator, swarm) = JsonNode::create_with_flags(timers);
    let mut joiner = JsonNode::join_with_flags(&swarm, "tk-silent", timers);
    assert!(creator.wait_ready(&swarm));
    assert!(joiner.wait_ready(&swarm));

    let saw_join = wait_until(
        || {
            creator
                .presence_events()
                .iter()
                .filter(|value| value["subtype"] == "joined")
                .count()
        },
        1,
        MSG_TIMEOUT,
    );
    assert!(saw_join >= 1, "creator never saw the joiner join");

    // Creator creates; the worker (joiner) holds the ball and accepts, then its
    // skill sends nothing more (simulating a crash/abandon while the daemon
    // process keeps running).
    let tid = common::cli_task_create(&swarm, &creator.nickname, "tk-silent", "port it");
    let saw_offer = wait_until(
        || {
            joiner
                .json_events()
                .iter()
                .filter(|value| value["event"] == "task" && value["kind"] == "message")
                .count()
        },
        1,
        MSG_TIMEOUT,
    );
    assert!(saw_offer >= 1, "joiner never surfaced the task message");
    common::cli_task_status(&swarm, "tk-silent", &tid, "working");

    // The ball-owner's daemon is still alive, but its skill is silent past the
    // keepalive-max window, so the keepalive stops and the task is reaped. The
    // ball-owner's own sweep and the creator's debounce fire at ~the same time
    // (whichever broadcasts `Cancel` first terminalizes the other), so accept a
    // `task_timeout` on **either** node — before the fix, none would ever fire.
    let timed_out = wait_until(
        || {
            let count = |node: &JsonNode| {
                node.json_events()
                    .iter()
                    .filter(|value| {
                        value["event"] == "task_timeout" && value["task_id"] == tid.as_str()
                    })
                    .count()
            };
            count(&creator) + count(&joiner)
        },
        1,
        RECOVERY_TIMEOUT,
    );
    assert!(
        timed_out >= 1,
        "a task whose ball-owner skill went silent must time out, even though the daemon lives"
    );
    assert!(
        joiner.child.try_wait().ok().flatten().is_none(),
        "the ball-owner daemon must still be alive — the timeout is from skill silence, not process death"
    );
}

/// `agent-gossip peers` returns the live roster: `ok`, a `count` (participants + 1
/// for self), and a `participants` array carrying nickname + recency +
/// quiet flag + reach (direct/gossip) for each known peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_peers_roster_shape() {
    let (creator, joiner_a, joiner_b, swarm) = three_peers("peers");

    let reach_values = |roster: &serde_json::Value| -> Vec<String> {
        roster["participants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["reach"].as_str().map(str::to_owned))
            .collect()
    };

    // Poll until the creator's roster has converged to both joiners *and*
    // at least one of them resolves to a live direct link — the reach tag is
    // populated from flooded PeerInfo, so it trails roster convergence.
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut roster = serde_json::Value::Null;
    while Instant::now() < deadline {
        roster = serde_json::from_str(&common::cli_peers(&swarm, &creator.nickname))
            .expect("peers response is JSON");
        if roster["participant_count"].as_u64() == Some(3)
            && reach_values(&roster).iter().any(|reach| reach == "direct")
        {
            break;
        }
        std::thread::sleep(POLL);
    }

    assert_eq!(roster["ok"], true);
    assert_eq!(roster["participant_count"], 3, "creator + 2 joiners");
    let participants = roster["participants"]
        .as_array()
        .expect("participants is an array");
    let nicks: Vec<&str> = participants
        .iter()
        .filter_map(|entry| entry["nickname"].as_str())
        .collect();
    assert!(nicks.contains(&joiner_a.nickname.as_str()));
    assert!(nicks.contains(&joiner_b.nickname.as_str()));
    // Each entry carries the documented fields, with a valid reach tag.
    for entry in participants {
        assert!(entry["nickname"].is_string());
        assert!(entry.get("last_seen_secs_ago").is_some());
        assert!(entry["quiet"].is_boolean());
        let reach = entry["reach"].as_str().expect("reach is a string");
        assert!(
            reach == "direct" || reach == "gossip",
            "unexpected reach: {reach}"
        );
    }
    assert!(
        reach_values(&roster).iter().any(|reach| reach == "direct"),
        "at least one peer should resolve to a live direct link in a localhost mesh"
    );

    // Regression: a joiner reaches the creator (the beacon) only through the
    // rendezvous link, which is kept out of `linked_endpoints`. The roster
    // must still tag the beacon `direct` — earlier this always read `gossip`
    // from a joiner's perspective.
    let creator_reach_from_joiner = |view: &serde_json::Value| -> Option<String> {
        view["participants"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["nickname"].as_str() == Some(creator.nickname.as_str()))
            .and_then(|entry| entry["reach"].as_str().map(str::to_owned))
    };
    let joiner_deadline = Instant::now() + MSG_TIMEOUT;
    let mut joiner_reach = None;
    while Instant::now() < joiner_deadline {
        let joiner_roster: serde_json::Value =
            serde_json::from_str(&common::cli_peers(&swarm, &joiner_a.nickname))
                .expect("peers response is JSON");
        joiner_reach = creator_reach_from_joiner(&joiner_roster);
        if joiner_reach.as_deref() == Some("direct") {
            break;
        }
        std::thread::sleep(POLL);
    }
    assert_eq!(
        joiner_reach.as_deref(),
        Some("direct"),
        "joiner should see the creator/beacon as a direct (rendezvous) link"
    );
}

/// Poll/stream parity: a `msg` returned by `agent-gossip poll --output json` is the
/// **byte-identical** object the live `--output json` stream emitted for the
/// same message — except for the leading `seq` the poll record adds as its
/// cursor. This is the contract a Monitor-less fallback relies on: parse one
/// shape, emit `display` verbatim, whether reading the stream or polling.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_poll_record_matches_stream_line() {
    let (creator, swarm) = JsonNode::create();
    let joiner = JsonNode::join(&swarm, "parity-joiner");
    assert!(creator.wait_ready(&swarm));
    assert!(joiner.wait_ready(&swarm));
    std::thread::sleep(Duration::from_secs(3));

    // Joiner sends; the creator surfaces it on its stream and can poll it.
    cli_send(&swarm, &joiner.nickname, "parity body");

    // Find the creator's stream line for this message.
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut stream_msg = None;
    while Instant::now() < deadline {
        stream_msg = creator
            .msg_events()
            .into_iter()
            .find(|value| value["body"] == "parity body");
        if stream_msg.is_some() {
            break;
        }
        std::thread::sleep(POLL);
    }
    let stream_msg = stream_msg.expect("creator stream should surface the message");

    // Poll the creator and find the same message.
    let poll_json = common::cli_poll(&swarm, &creator.nickname, None);
    let polled: Vec<serde_json::Value> =
        serde_json::from_str(&poll_json).expect("poll JSON parses");
    let mut poll_msg = polled
        .into_iter()
        .find(|value| value["body"] == "parity body")
        .expect("poll should return the message");

    // The poll record carries a `seq`; the stream line does not. Strip it and
    // the two objects must be identical (same fields, same values, incl.
    // `display` and `self`).
    assert!(poll_msg["seq"].is_u64(), "poll record must carry a seq");
    poll_msg
        .as_object_mut()
        .expect("poll record is an object")
        .remove("seq");
    assert_eq!(
        poll_msg, stream_msg,
        "poll record (sans seq) must byte-match the stream line"
    );
}

/// A `ping_report` is a transient event — never in the old message log — yet it
/// is now drainable via `poll`, so a Monitor-less fallback can still render the
/// RTT table. The originator of the ping surfaces the report on its own stream
/// *and* in its poll buffer.
#[test]
fn test_ping_report_is_pollable() {
    let (creator, swarm) = JsonNode::create_with_flags(&[("--ping-window-secs", "2")]);
    let joiner = JsonNode::join_with_flags(&swarm, "ping-pollee", &[]);
    assert!(creator.wait_ready(&swarm));
    assert!(joiner.wait_ready(&swarm));
    std::thread::sleep(Duration::from_secs(3));

    // Creator arms a ping; the report lands ~2s later on its stream and ring.
    common::cli_ping(&swarm, &creator.nickname);

    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut report = None;
    while Instant::now() < deadline {
        let poll_json = common::cli_poll(&swarm, &creator.nickname, None);
        if let Ok(polled) = serde_json::from_str::<Vec<serde_json::Value>>(&poll_json) {
            report = polled
                .into_iter()
                .find(|value| value["event"] == "ping_report");
            if report.is_some() {
                break;
            }
        }
        std::thread::sleep(POLL);
    }
    let report = report.expect("ping_report should be drainable via poll");

    assert!(report["seq"].is_u64(), "pollable ping_report carries a seq");
    assert!(
        report["display"]
            .as_str()
            .is_some_and(|line| line.contains("ping")),
        "ping_report display should render the RTT table: {report}"
    );
    assert!(
        report["peers"].is_array(),
        "ping_report carries the per-peer RTT rows: {report}"
    );
}

/// Shared-state wire contract over the REAL path the in-process harness bypasses:
/// `agent-gossip <channel> merge` on one daemon → the change gossips → the peer's
/// `--output json` stream carries a `{"event":"<chan>","type":"<chan>",...}`
/// record with the merge + derived document, and `agent-gossip <channel> get` on the peer
/// reflects it. Run for both channels (`state`, `meta`) to prove parity end to
/// end.
fn channel_wire_contract(channel: Channel) {
    let label = common::channel_subcommand(channel);
    let (creator, swarm) = JsonNode::create();
    let joiner = JsonNode::join(&swarm, &format!("wc-{label}-joiner"));
    assert!(creator.wait_ready(&swarm), "creator never served");
    assert!(joiner.wait_ready(&swarm), "joiner never served");
    // Let the two daemons mesh so the merge gossips live to the joiner.
    std::thread::sleep(Duration::from_secs(3));

    // Merge on the creator via the real CLI → Unix socket → daemon path.
    let merge = r#"{"k":"v"}"#;
    let out = common::cli_channel_merge(channel, &swarm, &creator.nickname, merge);
    assert!(
        out.status.success(),
        "{label} merge should exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let resp: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("merge stdout is JSON");
    assert_eq!(resp["ok"], true, "{label} merge should report ok:true");

    // The joiner surfaces it on its --output json stream as an `event:"<chan>"`
    // record carrying the merge document + the freshly-derived document.
    let want_doc = serde_json::json!({"k": "v"});
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut event = None;
    while Instant::now() < deadline {
        let poll = common::cli_poll(&swarm, &joiner.nickname, None);
        let records: Vec<serde_json::Value> =
            serde_json::from_str(&poll).expect("poll JSON parses");
        // Select OUR merge's event: on the meta channel the daemons' own
        // card publications (`/peers/<nick>/card`) also surface here.
        event = records
            .into_iter()
            .find(|record| record["event"] == label && record["merge"] == want_doc);
        if event.is_some() {
            break;
        }
        std::thread::sleep(POLL);
    }
    let event = event.unwrap_or_else(|| panic!("joiner never surfaced a {label} event"));
    assert_eq!(event["type"], label, "{label} event carries type:{label}");
    assert_eq!(
        event["merge"], want_doc,
        "{label} event carries the applied merge document: {event}"
    );
    // The derived document carries the merge; on the meta channel the
    // daemon-published cards are masked out of the comparison.
    let mut document = event["document"].clone();
    if let Some(map) = document.as_object_mut() {
        map.remove("peers");
    }
    assert_eq!(
        document, want_doc,
        "{label} event carries the derived document: {event}"
    );

    // The `<chan> get` command on the joiner returns the same document
    // (cards masked on meta, as above).
    let got = common::cli_channel_get(channel, &swarm, &joiner.nickname);
    let got: serde_json::Value = serde_json::from_str(&got).expect("get stdout is JSON");
    assert_eq!(got["ok"], true);
    let mut got_doc = got["document"].clone();
    if let Some(map) = got_doc.as_object_mut() {
        map.remove("peers");
    }
    assert_eq!(
        got_doc, want_doc,
        "{label} get on the peer reflects the creator's merge"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_wire_contract_patch_get_event_and_cas() {
    channel_wire_contract(Channel::State);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meta_wire_contract_patch_get_event_and_cas() {
    channel_wire_contract(Channel::Meta);
}
