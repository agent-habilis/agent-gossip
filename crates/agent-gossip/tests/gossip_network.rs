//! Integration tests for the gossip network.
//!
//! Each test spawns real `agent-gossip` processes, exercises the network,
//! and asserts on what each node actually received. Tests are independent —
//! each creates its own mesh so IPC sockets never collide.
//!
//! Crypto-heavy deps are optimized even in dev builds (see the
//! `[profile.dev.package]` overrides in `Cargo.toml`), so debug `cargo test`
//! runs at near-release connect speeds.
use std::collections::HashSet;
use std::fs::{self, File};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use agent_gossip_test_fixtures as common;
use common::{
    CONNECT_TIMEOUT, ChatMessage, InProcNode, KillOnDrop, MSG_TIMEOUT, Node, POLL, PresenceSubtype,
    RECOVERY_TIMEOUT, SHORT_EVICT, bin, chat_text, cli_message, cli_message_raw, cli_peers,
    cli_ping, cli_poll, cli_poll_long, cli_task_create_raw, ipc_raw, serial_guard, socket_path,
    tmp_log, trace_log, wait_until,
};
use habilis_network::util::process;
use habilis_network::util::process_start::ProcessStart;

/// Heal cadence for the reliability tests, injected through the hidden
/// `--heal-interval-secs` flag. Floored at 3s: below that the
/// claim-if-free walk and the probe timeouts get racy. Production stays
/// at 15s.
const TEST_HEAL_SECS: u64 = 3;

/// Anti-entropy cadence injected into the backfill tests via the hidden
/// `--backfill-interval-secs` flag (production default 10s).
const TEST_BACKFILL_SECS: u64 = 2;

/// Ceiling for the post-departure handoff poll. A co-host claim takes a
/// couple of heal cycles; the rest is margin for a loaded host. Callers
/// poll [`survivor_serves_rendezvous`], so they pay only the real handoff
/// time.
fn handoff_budget() -> Duration {
    Duration::from_secs(6 * TEST_HEAL_SECS + 20)
}

/// Blind post-departure migration wait for the tests whose asserts
/// need the handoff **fully settled** (`test_first_message_…`,
/// `test_creator_sigkill_…`). These run at the **production** heal
/// cadence: a short injected cadence makes the survivor co-host a
/// beacon *before* the old one dies, and because every beacon shares
/// the seed-derived endpoint id, the survivor's gossip slot for that
/// id then holds a zombie link to the dead beacon until iroh's idle
/// teardown clears it (30-60s, no close frames on process exit) — a
/// joiner arriving earlier bootstraps into a deaf beacon and its first
/// broadcast is unrecoverable. At the 15s production cadence the death
/// always precedes the first co-host tick, so the claim is clean; ≥2
/// cycles + margin is the empirically stable floor. Iroh-bound, not
/// heal-bound — do not shorten via `--heal-interval-secs`.
const RENDEZVOUS_HANDOFF: Duration = Duration::from_secs(36);

/// Whether this survivor can bootstrap a fresh joiner: it has bound a
/// rendezvous rung at least once (co-host or claim-if-free — every
/// beacon shares the seed-derived endpoint id, so which rung is
/// irrelevant) and its own rendezvous gossip link is currently up
/// (ups > downs), so the beacon is bridged into the surviving mesh,
/// not a bare socket.
///
/// Callers must first ensure the departed beacon's **process has
/// exited** (graceful `sigint` + `wait_exit`, or SIGKILL): the OS then
/// has released its sockets, so a joiner's rung-walk cannot dial a
/// dead-but-bound rung — the historical first-message-lost flake.
fn survivor_serves_rendezvous(mesh: &str, nick: &str) -> bool {
    let (assumed, ups, downs) = rendezvous_signals(mesh, nick);
    assumed && ups > downs
}

/// The two independent signals [`survivor_serves_rendezvous`] ANDs together:
/// did this survivor ever bind a rendezvous rung, and is its rendezvous gossip
/// link currently up.
fn rendezvous_signals(mesh: &str, nick: &str) -> (bool, usize, usize) {
    let trace = trace_log(mesh, nick);
    (
        trace.contains("beacon assumed"),
        rendezvous_links(&trace, "gossip neighbor up"),
        rendezvous_links(&trace, "gossip neighbor down"),
    )
}

fn rendezvous_links(trace: &str, direction: &str) -> usize {
    let matching = trace
        .lines()
        .filter(|line| line.contains(direction) && line.contains("is_rendezvous=true"));
    matching.count()
}

/// Why a survivor is not serving the rendezvous, for assertion messages. The
/// JSON stdout log cannot answer this — a mesh that healed fine still prints a
/// clean roster — so a bare stdout tail left CI handoff failures unreadable.
fn rendezvous_diagnosis(mesh: &str, nick: &str) -> String {
    let (assumed, ups, downs) = rendezvous_signals(mesh, nick);
    let trace = trace_log(mesh, nick);
    let mut tail: Vec<&str> = trace.lines().rev().take(25).collect();
    tail.reverse();
    format!(
        "<{nick}>: beacon assumed={assumed}, rendezvous links up={ups} down={downs} \
         (needs assumed=true and up>down)\ntrace tail:\n{}",
        tail.join("\n")
    )
}

/// Poll until any survivor serves the rendezvous (see
/// [`survivor_serves_rendezvous`]). Returns whether that landed within
/// [`handoff_budget`].
fn wait_rendezvous_served(mesh: &str, survivors: &[&str]) -> bool {
    wait_until(
        || {
            survivors
                .iter()
                .filter(|nick| survivor_serves_rendezvous(mesh, nick))
                .count()
        },
        1,
        handoff_budget(),
    ) >= 1
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Every version surface reports the same string, and it leads with the **app**
/// crate's version. `doctor` used to read the engine crate's `VERSION` const,
/// so a shipped 0.7.2 binary answered `--version` with 0.7.2 and `doctor` with
/// the engine's 0.5.0. Asserting the two agree needs a real process — clap
/// renders `--version` itself — and pinning the prefix is what catches a
/// re-slip to the engine's number, since the two consts share a git stamp and
/// differ only in the leading version.
#[test]
fn version_surfaces_agree_and_lead_with_the_app_crate() {
    let version_out = common::test_cmd()
        .arg("--version")
        .output()
        .expect("--version failed to spawn");
    let rendered = String::from_utf8_lossy(&version_out.stdout);
    let stamp = rendered
        .trim()
        .strip_prefix("agent-gossip ")
        .expect("--version renders as `agent-gossip <stamp>`")
        .to_owned();

    assert_eq!(
        stamp,
        agent_gossip::version(),
        "`--version` and the library's version() disagree"
    );
    assert!(
        stamp.starts_with(env!("CARGO_PKG_VERSION")),
        "version must lead with the app crate's version {}, got: {stamp}",
        env!("CARGO_PKG_VERSION")
    );

    let doctor_out = common::test_cmd()
        .args(["doctor", "--no-probe", "--output", "json"])
        .output()
        .expect("doctor failed to spawn");
    let doctor = String::from_utf8_lossy(&doctor_out.stdout);
    assert!(
        doctor.contains(&stamp),
        "doctor reports a different version than `--version` ({stamp}):\n{doctor}"
    );
}

/// Wire contract for the `info` IPC + `doctor` active-meshes scan: a live
/// daemon answers `info` over its socket with its own identity, so
/// `doctor` lists it under "Active gossips" with the full mesh
/// id and name. `--no-probe` keeps this to the fast local-socket scan (no
/// net-report), so the test stays offline-safe.
#[test]
fn doctor_lists_active_mesh_as_json() {
    let _serial = serial_guard();

    let (node, mesh) = Node::create_named("itest-doctor");
    assert!(node.wait_ready(&mesh), "daemon never ready");

    let out: std::process::Output = common::test_cmd()
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
        stdout.contains("Active gossips"),
        "no Active gossips section:\n{stdout}"
    );
    assert!(
        stdout.contains(mesh.as_str()),
        "active mesh {mesh} not listed:\n{stdout}"
    );
    assert!(
        stdout.contains("itest-doctor"),
        "mesh name not listed:\n{stdout}"
    );
}

/// Basic sanity: a broadcast message is received by the non-sending node.
/// The node whose IPC socket is used will NOT receive its own broadcast;
/// the peer will. We check total delivery across both nodes = 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_two_node_message_delivery() {
    let creator = InProcNode::create("net2node").await;
    let mut joiner = InProcNode::join(&creator.mesh, "joiner-2node").await;

    creator.broadcast("hello from the network").await;

    assert!(
        joiner.wait_inbound(1, MSG_TIMEOUT).await,
        "joiner never received the creator's message"
    );
    let received: Vec<_> = joiner.inbound().collect();
    assert_eq!(received.len(), 1, "expected exactly 1 delivery");
    assert_eq!(
        chat_text(received[0]),
        "hello from the network",
        "the joiner receives the one message"
    );
}

/// A passworded mesh: the right password joins and messages flow; a wrong
/// password fails locally against the id's verifier ("wrong password"), and
/// no password at all is a typed "password-protected" error — both before
/// any network traffic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_passworded_mesh_verifies_locally_and_meshes() {
    let creator = InProcNode::create_with_password("netpw", "hunter2").await;

    // Wrong password: rejected by the verifier carried in the id.
    let Err(wrong) =
        InProcNode::try_join_with_password(&creator.mesh, "joiner-bad", "hunter3").await
    else {
        panic!("a wrong password must be rejected");
    };
    assert!(wrong.to_string().contains("wrong password"), "got: {wrong}");

    // No password: a crisp requirement error, not a silent empty mesh.
    let target: agent_gossip::JoinTarget = creator
        .mesh
        .parse()
        .expect("a mesh id printed by a creator is always a well-formed join target");
    let missing = agent_gossip::api::MeshSession::join(agent_gossip::api::JoinConfig::new(target))
        .await
        .expect_err("a missing password must be rejected");
    assert!(
        missing.to_string().contains("password-protected"),
        "got: {missing}"
    );

    // The right password lands on the same topic and messages flow.
    let mut joiner = InProcNode::try_join_with_password(&creator.mesh, "joiner-good", "hunter2")
        .await
        .expect("the right password joins");
    creator.broadcast("hello behind the password").await;
    assert!(
        joiner.wait_inbound(1, MSG_TIMEOUT).await,
        "passworded joiner never received the creator's message"
    );
    let inbound: Vec<_> = joiner.inbound().collect();
    assert_eq!(
        chat_text(inbound[0]),
        "hello behind the password",
        "the password joiner receives the message"
    );
}

/// The subprocess wire contract for `--password`: `create --password=pw`
/// mints a passworded id; `join` without the password exits non-zero with
/// "password-protected" under `--no-interactive`, a wrong password exits
/// non-zero with "wrong password", and `join --password=pw` meshes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_password_flag_wire_contract() {
    let (creator, mesh_id) = Node::create_args("pwwire", &["--password=hunter2"], &[]);
    assert!(creator.wait_ready(&mesh_id), "creator never became ready");

    // Missing password, non-interactive: crisp requirement error.
    let missing = common::test_cmd()
        .args(["join", &mesh_id])
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
        .args(["join", &mesh_id, "--password=hunter3"])
        .output()
        .expect("spawn join with wrong password");
    assert!(!wrong.status.success(), "wrong password must exit non-zero");
    let wrong_stderr = String::from_utf8_lossy(&wrong.stderr);
    assert!(
        wrong_stderr.contains("wrong password"),
        "stderr must say wrong password, got: {wrong_stderr}"
    );

    // The right password joins and meshes (the join's ready socket appears).
    let joiner = Node::join_args(&mesh_id, "pw-joiner", &["--password=hunter2"], &[]);
    assert!(
        joiner.wait_ready(&mesh_id),
        "passworded joiner never became ready: {}",
        joiner.log_tail(20)
    );
    joiner.kill();
    creator.kill();
}

/// Three-node full-mesh: a broadcast reaches the 2 other nodes (the sender
/// never receives its own broadcast).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_three_node_full_delivery() {
    let alpha = InProcNode::create("net3node").await;
    let mut beta = InProcNode::join(&alpha.mesh, "beta-3node").await;
    let mut gamma = InProcNode::join(&alpha.mesh, "gamma-3node").await;

    alpha.broadcast("broadcast to all three nodes").await;

    // The sender never receives its own broadcast; the other two
    // should (>=2 deliveries across beta+gamma).
    assert!(
        beta.wait_inbound(1, MSG_TIMEOUT).await && gamma.wait_inbound(1, MSG_TIMEOUT).await,
        "expected both beta and gamma to receive the broadcast"
    );
    for message in beta.inbound().chain(gamma.inbound()) {
        assert_eq!(
            chat_text(message),
            "broadcast to all three nodes",
            "every peer receives the broadcast"
        );
    }
}

/// Send from both sides and verify each receives the other's message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bidirectional_messaging() {
    let mut creator = InProcNode::create("netbidir").await;
    let mut joiner = InProcNode::join(&creator.mesh, "joiner-bidir").await;

    // Each side posts as itself — creator receives joiner's message
    // and vice versa.
    creator.broadcast("first message").await;
    joiner.broadcast("second message").await;

    assert!(
        joiner.wait_body("first message", MSG_TIMEOUT).await,
        "joiner never received the creator's message"
    );
    assert!(
        creator.wait_body("second message", MSG_TIMEOUT).await,
        "creator never received the joiner's message"
    );
}

/// A cap under the minimum of the engine ends the start with the engine text and a non-zero exit.
/// The process is the daemon in the foreground, so a process that ended leaves no daemon.
#[test]
fn a_cap_under_the_minimum_is_refused_at_start_with_the_engine_text() {
    for cap in ["0", "4", "7"] {
        let mut command = common::test_cmd();
        command.args([
            "create",
            "--max-connections",
            cap,
            "--nickname",
            "cap-refused",
        ]);
        let out: std::process::Output = output_within(&mut command, QUERY_LIMIT);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let text = format!("max connections is {cap}: the lowest value is 8 (the default is 16)");
        assert!(
            !out.status.success() && stderr.contains(&text),
            "--max-connections {cap} must end the start with `{text}`, and it gave {out:?}"
        );
    }
}

/// The help of `--max-connections` says what the engine enforces: the lowest value and the default.
#[test]
fn the_help_of_max_connections_names_the_lowest_value_and_the_default() {
    let mut command = common::test_cmd();
    command.args(["create", "--help"]);
    let out: std::process::Output = output_within(&mut command, QUERY_LIMIT);
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("The lowest value is 8") && help.contains("[default: 16]"),
        "the help of --max-connections names the lowest value 8 and the default 16: {help}"
    );
}

/// A detached start that the engine refuses leaves `ready` with the reason at once: the launcher
/// returns, the daemon has ended with its error, and `ready --state-file` says so without its wait.
#[test]
fn ready_reports_a_refused_detached_start_at_once() {
    let dir = std::env::temp_dir().join(format!("ready-refused-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp dir is writable");
    let state_file = dir.join("session.json");
    let owner = process::parent_of(std::process::id()).expect("the test process has a parent");
    let mut launch = common::test_cmd();
    launch.args([
        "create",
        "--max-connections",
        "4",
        "--nickname",
        "ready-refused",
        "--owner-pid",
        &owner.to_string(),
        "--state-file",
    ]);
    launch.arg(&state_file);
    let launched: std::process::Output = output_within(&mut launch, QUERY_LIMIT);
    assert!(
        launched.status.success(),
        "the detached launcher returns 0 before the daemon is checked: {launched:?}"
    );

    let started = Instant::now();
    let mut ready = common::test_cmd();
    ready.args(["ready", "--timeout-secs", "20", "--state-file"]);
    ready.arg(&state_file);
    let out: std::process::Output = output_within(&mut ready, Duration::from_secs(30));
    let waited = started.elapsed();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = "max connections is 4: the lowest value is 8 (the default is 16)";
    assert!(
        !out.status.success() && stderr.contains(text) && waited < Duration::from_secs(10),
        "ready must fail within 10 s with `{text}`, and it gave {out:?} after {waited:?}"
    );
}

/// A retry on the state file of a start that failed is not judged by the old error. The skills
/// start the daemon and `ready` together on one path, so `ready` can look before the new daemon has
/// written anything, and the file still holds the error of the start before.
#[test]
fn ready_is_not_misled_by_the_error_of_an_earlier_start_on_the_same_path() {
    let dir = std::env::temp_dir().join(format!("ready-retry-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp dir is writable");
    let state_file = dir.join("session.json");
    let mut refused = common::test_cmd();
    refused.args([
        "create",
        "--max-connections",
        "4",
        "--nickname",
        "retry-a",
        "--state-file",
    ]);
    refused.arg(&state_file);
    let first: std::process::Output = output_within(&mut refused, QUERY_LIMIT);
    assert!(
        !first.status.success(),
        "the first start must be refused by the cap: {first:?}"
    );
    // Longer than the grace that `ready` allows before its own start.
    std::thread::sleep(Duration::from_secs(4));

    let mut ready = common::test_cmd();
    ready.args(["ready", "--timeout-secs", "60", "--state-file"]);
    ready
        .arg(&state_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let waiting = ready.spawn().expect("ready starts");
    std::thread::sleep(Duration::from_millis(300));
    let log = tmp_log("retry-b");
    let file = File::create(&log).expect("the test temp dir is writable");
    let mut start = common::test_cmd();
    start.args([
        "create",
        "--name",
        "retry-b",
        "--lookup",
        "loopback",
        "--state-file",
    ]);
    start.arg(&state_file);
    let log_out = Stdio::from(file.try_clone().expect("the log file handle can be cloned"));
    start.stdout(log_out).stderr(Stdio::from(file));
    let child: Child = start.spawn().expect("the second start spawns");
    let _daemon = KillOnDrop::new(child);
    let out = waiting.wait_with_output().expect("ready ends");
    assert!(
        out.status.success(),
        "ready must wait for the daemon of the retry and end 0, and it gave {out:?}\ndaemon log:\n{}",
        fs::read_to_string(&log).unwrap_or_default()
    );
}

/// A broadcast with no running server exits non-zero with a clear error message.
#[test]
fn test_no_server_error() {
    // All-`1` Base58 payload — valid charset, can't match a real mesh.
    let fake_mesh = "1111111111111111111111111111111111111111111111111111111111111";
    let out = cli_message_raw(fake_mesh, "ghost-nick", "hello");
    assert!(
        !out.status.success(),
        "expected non-zero exit when no server is running"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("No active gossip server"),
        "expected 'No active gossip server' in stderr, got: {stderr}"
    );
}

/// A body over the single-message cap is transparently split and delivered —
/// including past the old 16-shard ceiling, which no longer exists. (The
/// too-large refusal now sits at `MAX_LOGICAL_BODY_BYTES` (64 `MiB`), beyond
/// what an OS argv can carry, so it is covered in-process in `multipart.rs`.)
#[test]
fn test_oversize_body_splits_and_reassembles() {
    let (creator, mesh) = Node::create();
    let joiner = Node::join(&mesh, "joiner-mp");
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    assert!(joiner.wait_ready(&mesh), "joiner socket never appeared");

    // 32x the frame cap needs ~35 shards — over the old 16-shard limit — and
    // the receiver still reassembles it, so it surfaces once as the whole
    // body on each stream: the sender's self-echo and the peer's delivery.
    let body = "a".repeat(agent_gossip::MAX_MESSAGE_SIZE * 32);
    let _ = cli_message(&mesh, &creator.nickname, &body);
    let total = wait_until(
        || creator.messages().count() + joiner.messages().count(),
        2,
        MSG_TIMEOUT,
    );
    assert_eq!(
        total, 2,
        "the multipart body surfaces exactly once per node"
    );
    let mut got: Vec<ChatMessage> = creator.messages().collect();
    got.extend(joiner.messages());
    for message in &got {
        assert_eq!(
            message.body, body,
            "the reassembled body matches the original"
        );
    }
}

/// UTF-8 message bodies (accents, emoji, CJK) are accepted and delivered verbatim.
#[test]
fn test_utf8_body_round_trip() {
    let (creator, mesh) = Node::create();
    let joiner = Node::join(&mesh, "joiner-utf8");
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    assert!(joiner.wait_ready(&mesh), "joiner socket never appeared");

    let body = "héllo 💬 日本語";
    let _ = cli_message(&mesh, &creator.nickname, body);

    // One send surfaces twice: the sender's self-echo + the peer's delivery.
    let total = wait_until(
        || creator.messages().count() + joiner.messages().count(),
        2,
        MSG_TIMEOUT,
    );
    assert_eq!(total, 2, "utf-8 message delivered verbatim on both streams");
    let mut msgs: Vec<ChatMessage> = creator.messages().collect();
    msgs.extend(joiner.messages());
    for message in &msgs {
        assert_eq!(message.body, body, "the message body is kept as sent");
    }
}

/// Control characters (other than tab/newline) in a body are rejected.
#[test]
fn test_control_char_body_rejected() {
    let (creator, mesh) = Node::create();
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");

    let out = cli_message_raw(&mesh, &creator.nickname, "bad\u{7}bell");
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

/// Several broadcasts in a row all get distinct ids and all arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_concurrent_asks() {
    const ASK_COUNT: usize = 5;
    let mut creator = InProcNode::create("netconc").await;
    let joiner = InProcNode::join(&creator.mesh, "joiner-concurrent").await;

    let mut ids = HashSet::new();
    for index in 0..ASK_COUNT {
        let id = joiner
            .broadcast(&format!("concurrent message {index}"))
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

/// With three agents on the same mesh, a message carries the nickname of the
/// agent that sent it, on every receiver.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_ask_targets_specific_agent_three_peers() {
    let alpha = InProcNode::create("netpick3").await;
    let mut beta = InProcNode::join(&alpha.mesh, "beta-pick3").await;
    let mut gamma = InProcNode::join(&alpha.mesh, "gamma-pick3").await;

    // Each node posts a uniquely-tagged message as itself; the other
    // two must receive it with the *correct* author (each in-process
    // session owns its own nickname, so authorship can't be swapped).
    let alpha_nick = alpha.nickname.clone();
    alpha.broadcast("tag-from-alpha").await;
    for node in [&mut beta, &mut gamma] {
        assert!(
            node.wait_body("tag-from-alpha", MSG_TIMEOUT).await,
            "alpha's message not delivered"
        );
        let message = node
            .inbound()
            .into_iter()
            .find(|message| chat_text(message) == "tag-from-alpha")
            .expect("alpha's message missing");
        assert_eq!(
            message.author.to_string(),
            alpha_nick,
            "wrong author for alpha's message"
        );
    }

    beta.broadcast("tag-from-beta").await;
    gamma.broadcast("tag-from-gamma").await;
    assert!(
        beta.wait_body("tag-from-gamma", MSG_TIMEOUT).await,
        "gamma's message not delivered to beta"
    );
    assert!(
        gamma.wait_body("tag-from-beta", MSG_TIMEOUT).await,
        "beta's message not delivered to gamma"
    );
    let gamma_msg = beta
        .inbound()
        .into_iter()
        .find(|message| chat_text(message) == "tag-from-gamma")
        .expect("gamma msg missing on beta");
    assert_eq!(
        gamma_msg.author.to_string(),
        gamma.nickname,
        "wrong author for gamma's message"
    );
}

/// Graceful shutdown: SIGINT fires the ctrl-c handler which broadcasts a `left`
/// presence message before exiting. We verify the creator receives the `left` event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_graceful_shutdown_handler_fires() {
    let mut creator = InProcNode::create("netshutdown").await;
    let joiner = InProcNode::join(&creator.mesh, "joiner-shutdown").await;

    // Let the mesh form so the `Left` broadcast has a live link.
    assert!(
        creator
            .wait_presence("joiner-shutdown", PresenceSubtype::Joined, MSG_TIMEOUT)
            .await,
        "joiner never surfaced as joined before leaving"
    );

    // Graceful leave broadcasts a `Left` presence.
    joiner.leave().await;

    assert!(
        creator
            .wait_presence("joiner-shutdown", PresenceSubtype::Left, MSG_TIMEOUT)
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

    let alpha = InProcNode::join(&observer.mesh, "alpha-order").await;
    assert!(
        observer
            .wait_presence_count(PresenceSubtype::Joined, 1, MSG_TIMEOUT)
            .await,
        "alpha joined not received"
    );

    let beta = InProcNode::join(&observer.mesh, "beta-order").await;
    assert!(
        observer
            .wait_presence_count(PresenceSubtype::Joined, 2, MSG_TIMEOUT)
            .await,
        "beta joined not received"
    );

    alpha.leave().await;
    assert!(
        observer
            .wait_presence_count(PresenceSubtype::Left, 1, MSG_TIMEOUT)
            .await,
        "alpha left not received"
    );

    let gamma = InProcNode::join(&observer.mesh, "gamma-order").await;
    assert!(
        observer
            .wait_presence_count(PresenceSubtype::Joined, 3, MSG_TIMEOUT)
            .await,
        "gamma joined not received"
    );

    beta.leave().await;
    assert!(
        observer
            .wait_presence_count(PresenceSubtype::Left, 2, MSG_TIMEOUT)
            .await,
        "beta left not received"
    );

    gamma.leave().await;
    assert!(
        observer
            .wait_presence_count(PresenceSubtype::Left, 3, MSG_TIMEOUT)
            .await,
        "gamma left not received"
    );

    assert!(
        observer.presence_count(PresenceSubtype::Joined) >= 3,
        "expected >=3 joined, got {}",
        observer.presence_count(PresenceSubtype::Joined)
    );
    assert!(
        observer.presence_count(PresenceSubtype::Left) >= 3,
        "expected >=3 left, got {}",
        observer.presence_count(PresenceSubtype::Left)
    );
}

/// `--lookup mdns,dht,relay` is accepted and the node starts successfully.
#[test]
fn test_lookup_list_accepted() {
    let log = tmp_log("public");
    let file = File::create(&log).expect("the test log directory is writable");
    let stdout = Stdio::from(file.try_clone().expect("the test log file handle is open"));
    let mut command = common::test_cmd();
    command.args(["create", "--name", "pub-test", "--lookup", "mdns,dht,relay"]);
    let mut child: Child = command
        .stdout(stdout)
        .stderr(Stdio::from(file))
        .spawn()
        .expect("failed to spawn create --lookup mdns,dht,relay");

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

    assert!(
        found,
        "create --lookup mdns,dht,relay did not produce any output"
    );
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
        let file = File::create(&log).expect("the test log directory is writable");
        let state_file = std::env::temp_dir().join(format!(
            "agent-gossip-statefile-test-{}-{signal}.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&state_file);

        let stdout = Stdio::from(file.try_clone().expect("the test log file handle is open"));
        let mut command = common::test_cmd();
        command
            .args(["create", "--name", "statefile-test", "--lookup", "loopback"])
            .arg("--state-file")
            .arg(&state_file);
        let mut child: Child = command
            .stdout(stdout)
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

/// `agent-gossip ready --state-file PATH` is the CLI-fallback readiness gate: it blocks
/// until the daemon writing PATH flips the file's `ready` flag to true (set
/// only once the event loop is serving), then exits 0. This covers the gate
/// against an already-up daemon and asserts the file then carries `ready:true`
/// plus the minted identity the caller reads next.
#[test]
fn test_ready_gate_succeeds_when_serving() {
    let log = tmp_log("ready-before");
    let file = File::create(&log).expect("the test log directory is writable");
    let state_file = std::env::temp_dir().join(format!(
        "agent-gossip-ready-before-{}.json",
        std::process::id()
    ));
    let _ = fs::remove_file(&state_file);

    let stdout = Stdio::from(file.try_clone().expect("the test log file handle is open"));
    let mut command = common::test_cmd();
    command
        .args(["create", "--name", "ready-test", "--lookup", "loopback"])
        .arg("--state-file")
        .arg(&state_file);
    let mut child: Child = command
        .stdout(stdout)
        .stderr(Stdio::from(file))
        .spawn()
        .expect("failed to spawn create --state-file");

    // Gate against the live daemon: exits 0 once it is serving.
    command = common::test_cmd();
    command
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "60"]);
    let status: std::process::ExitStatus =
        command.status().expect("failed to run agent-gossip ready");
    assert!(
        status.success(),
        "agent-gossip ready should exit 0 against a serving daemon\nlog:\n{}",
        fs::read_to_string(&log).unwrap_or_default()
    );

    // The gate returning means the file is complete and serving — the caller
    // reads identity + ready:true from it.
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_file).expect("the test file is readable"))
            .expect("the state file holds JSON written by the daemon");
    assert_eq!(parsed["ready"], true, "gate returned but ready is not true");
    assert_eq!(
        parsed["name"], "ready-test",
        "the ready output names the ready test"
    );
    assert!(
        parsed["gossip"]
            .as_str()
            .is_some_and(|mesh| agent_gossip::MeshId::try_new(mesh).is_ok()),
        "the gate reports ready"
    );
    assert!(
        parsed["nickname"].as_str().is_some(),
        "the ready output carries a nickname"
    );

    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();
    let _ = fs::remove_file(&log);
    let _ = fs::remove_file(&state_file);
}

/// Two daemons must not share one `--state-file`.
///
/// A state file has a single writer: the daemon replaces it wholesale every
/// heartbeat. Pointed at one path, two daemons take turns owning it, and the
/// bell, `ready`, `session` and `leave` all resolve identity from that file —
/// so which agent is reachable flips per heartbeat with nothing to say so.
/// The skills derive the path from the agent's parent pid, which makes two
/// sessions under one parent collide by construction. The second daemon must
/// refuse to start, loudly, rather than join the rotation.
#[test]
fn second_daemon_refuses_a_state_file_already_in_use() {
    let log = tmp_log("statefile-collide");
    let file = File::create(&log).expect("the test log directory is writable");
    let state_file =
        std::env::temp_dir().join(format!("agent-gossip-collide-{}.json", std::process::id()));
    let _ = fs::remove_file(&state_file);

    let stdout = Stdio::from(file.try_clone().expect("the test log file handle is open"));
    let mut command = common::test_cmd();
    command
        .args(["create", "--name", "collide-first", "--lookup", "loopback"])
        .arg("--state-file")
        .arg(&state_file);
    let mut first: Child = command
        .stdout(stdout)
        .stderr(Stdio::from(file))
        .spawn()
        .expect("the test binary is built before the suite runs, so its path always spawns");

    // Wait until the first daemon owns the file and is serving.
    command = common::test_cmd();
    command
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "60"]);
    let status: std::process::ExitStatus =
        command.status().expect("failed to run agent-gossip ready");
    assert!(status.success(), "the first daemon never came up");

    // Spawn rather than `output()`: a daemon that is *not* refused never
    // exits, so waiting on it would hang the suite instead of failing it.
    command = common::test_cmd();
    command
        .args(["create", "--name", "collide-second", "--lookup", "loopback"])
        .arg("--state-file")
        .arg(&state_file);
    let mut second: Child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the test binary is built before the suite runs, so its path always spawns");

    let deadline = Instant::now() + Duration::from_secs(30);
    let exit = loop {
        match second.try_wait().expect("failed to poll the second daemon") {
            Some(exit) => break exit,
            None if Instant::now() >= deadline => {
                let _ = second.kill();
                let _ = second.wait();
                let _ = Command::new("kill")
                    .args(["-TERM", &first.id().to_string()])
                    .status();
                let _ = first.wait();
                panic!("the second daemon kept running on a state file already in use");
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    assert!(!exit.success(), "the second daemon exited 0");
    let mut stderr = String::new();
    if let Some(mut pipe) = second.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stderr);
    }
    // The refusal is the engine's: the key lock of the state file is held. The CLI has no check of its own.
    assert!(
        stderr.contains("another daemon still runs on this state file"),
        "the refusal should say that a daemon runs on the file, got: {stderr}"
    );

    // The first daemon still owns the file and is unharmed.
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_file).expect("the test file is readable"))
            .expect("the state file holds JSON written by the daemon");
    assert_eq!(
        parsed["name"], "collide-first",
        "the collision keeps the first name"
    );

    let _ = Command::new("kill")
        .args(["-TERM", &first.id().to_string()])
        .status();
    let _ = first.wait();
    let _ = fs::remove_file(&log);
    let _ = fs::remove_file(&state_file);
}

/// The race the gate exists for: `agent-gossip ready` is started *before* the daemon, so
/// the state file does not exist yet. The gate must block (file-appears, then
/// ready-flips) and still exit 0 once the daemon comes up and serves.
#[test]
fn test_ready_gate_waits_for_a_late_daemon() {
    let log = tmp_log("ready-after");
    let file = File::create(&log).expect("the test log directory is writable");
    let state_file = std::env::temp_dir().join(format!(
        "agent-gossip-ready-after-{}.json",
        std::process::id()
    ));
    let _ = fs::remove_file(&state_file);

    // Start the gate first — nothing has written the file yet.
    let mut command = common::test_cmd();
    command
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "60"]);
    let mut gate: Child = command.spawn().expect("failed to spawn agent-gossip ready");

    // Launch the daemon a beat later, writing the same state file.
    std::thread::sleep(Duration::from_millis(500));
    let stdout = Stdio::from(file.try_clone().expect("the test log file handle is open"));
    command = common::test_cmd();
    command
        .args(["create", "--name", "ready-race", "--lookup", "loopback"])
        .arg("--state-file")
        .arg(&state_file);
    let mut child: Child = command
        .stdout(stdout)
        .stderr(Stdio::from(file))
        .spawn()
        .expect("failed to spawn create --state-file");

    let status = gate.wait().expect("agent-gossip ready never exited");
    assert!(
        status.success(),
        "agent-gossip ready started before the daemon should still exit 0 once it serves\nlog:\n{}",
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
/// and exit non-zero (the `failed to {create,join} mesh` contract the skills
/// rely on).
#[test]
fn test_ready_gate_times_out_without_a_daemon() {
    let state_file = std::env::temp_dir().join(format!(
        "agent-gossip-ready-timeout-{}-never.json",
        std::process::id()
    ));
    let _ = fs::remove_file(&state_file);

    let mut command = common::test_cmd();
    command
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "2"]);
    let status: std::process::ExitStatus =
        command.status().expect("failed to run agent-gossip ready");
    assert!(
        !status.success(),
        "agent-gossip ready should exit non-zero when no daemon ever writes the state file"
    );
}

/// A stale `ready:true` left by a prior daemon killed with SIGKILL must NOT
/// satisfy the gate: the gate checks `last_updated` freshness, so an old
/// timestamp (no live daemon refreshing it) is rejected and the gate times
/// out. Without the freshness check this file would be a false-positive ready.
#[test]
fn test_ready_gate_rejects_a_stale_ready_file() {
    let state_file = std::env::temp_dir().join(format!(
        "agent-gossip-ready-stale-{}.json",
        std::process::id()
    ));
    // ready:true but last_updated far in the past (well beyond READY_FRESH_SECS).
    fs::write(
        &state_file,
        r#"{"last_updated":1000000000,"name":"stale","nickname":"old-nick","peer_count":1,"ready":true,"gossip":"deadbeef"}"#,
    )
    .expect("the temp directory is writable");

    let mut command = common::test_cmd();
    command
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "2"]);
    let status: std::process::ExitStatus =
        command.status().expect("failed to run agent-gossip ready");
    assert!(
        !status.success(),
        "agent-gossip ready must reject a stale ready:true file (last_updated too old) and time out"
    );
    let _ = fs::remove_file(&state_file);
}

/// `agent-gossip ready` doubles as the identity read: on a fresh
/// `ready:true` file it prints `{mesh,name,nickname}` and exits 0, so a
/// fallback caller learns its own identity from the gate without parsing the
/// state file (or guessing its `${PPID}` name) itself.
#[test]
fn test_ready_gate_emits_identity_json_on_success() {
    let state_file = std::env::temp_dir().join(format!(
        "agent-gossip-ready-json-{}.json",
        std::process::id()
    ));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the system clock is after the Unix epoch")
        .as_secs();
    fs::write(
        &state_file,
        format!(
            r#"{{"last_updated":{now},"name":"cool-team","nickname":"calm-otter","peer_count":1,"ready":true,"gossip":"deadbeef"}}"#
        ),
    )
    .expect("the temp directory is writable");

    let mut command = common::test_cmd();
    command
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "5"]);
    let output: std::process::Output = command.output().expect("failed to run agent-gossip ready");
    assert!(
        output.status.success(),
        "a fresh ready:true file should pass the gate"
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ready prints a JSON object");
    assert_eq!(parsed["gossip"], "deadbeef", "ready prints the gossip id");
    assert_eq!(parsed["name"], "cool-team", "ready prints the name");
    assert_eq!(
        parsed["nickname"], "calm-otter",
        "ready prints the nickname"
    );
    let _ = fs::remove_file(&state_file);
}

#[test]
fn test_ready_gate_prints_the_author_pubkey() {
    let pubkey = "ab".repeat(32);
    let state_file = std::env::temp_dir().join(format!(
        "agent-gossip-ready-pubkey-{}.json",
        std::process::id()
    ));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the system clock is after the Unix epoch")
        .as_secs();
    fs::write(
        &state_file,
        format!(
            r#"{{"last_updated":{now},"name":"cool-team","nickname":"calm-otter","peer_count":1,"pubkey":"{pubkey}","ready":true,"gossip":"deadbeef"}}"#
        ),
    )
    .expect("the temp directory is writable");

    let mut command = common::test_cmd();
    command
        .arg("ready")
        .arg("--state-file")
        .arg(&state_file)
        .args(["--timeout-secs", "5"]);
    let output: std::process::Output = command.output().expect("failed to run agent-gossip ready");
    assert!(
        output.status.success(),
        "a fresh ready:true file should pass the gate"
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ready prints a JSON object");
    assert_eq!(
        parsed["pubkey"], pubkey,
        "ready prints the author pubkey, the key the meta slot is keyed by"
    );
    let _ = fs::remove_file(&state_file);
}

/// The poll command retrieves buffered events from a running mesh process,
/// each carrying its surfacing `seq`. Calling poll with `--after <seq>` returns
/// only events surfaced after that seq. The records are the same shape the live
/// `--output json` stream emits (`event`/`type`/`display`/`self`), so a fallback
/// agent parses one shape whether it reads the stream or polls.
#[test]
fn test_poll_returns_messages() {
    let (creator, mesh) = Node::create();
    let joiner = Node::join(&mesh, "joiner-poll");
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    assert!(joiner.wait_ready(&mesh), "joiner socket never appeared");

    // Wait for presence to settle.
    std::thread::sleep(Duration::from_secs(2));

    // Send two messages via joiner's IPC.
    let _ = cli_message(&mesh, &joiner.nickname, "hello from poll test");
    std::thread::sleep(Duration::from_millis(500));
    let _ = cli_message(&mesh, &joiner.nickname, "second message");
    std::thread::sleep(Duration::from_millis(500));

    // Poll all events from joiner's process.
    let all_json = cli_poll(&mesh, &joiner.nickname, None);
    let all: Vec<serde_json::Value> = serde_json::from_str(&all_json)
        .unwrap_or_else(|error| panic!("failed to parse poll JSON: {error}\nraw: {all_json}"));

    // Should have at least the 2 messages we sent (plus possible presence).
    let msg_bodies: Vec<&str> = all
        .iter()
        .filter_map(|message| message["body"].as_str())
        .collect();
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
    assert_eq!(
        first["event"], "message",
        "the first poll event is a message"
    );
    assert_eq!(
        first["type"], "broadcast",
        "the first poll event is a broadcast"
    );
    assert_eq!(
        first["display"], "💬️ `<joiner-poll>`: hello from poll test",
        "the poll display names the joiner"
    );
    assert_eq!(first["self"], true, "joiner authored it → self:true");

    // Poll with `--after <seq>` of the first message → excludes it, keeps the
    // second (the unified seq cursor).
    let first_seq = first["seq"]
        .as_u64()
        .expect("the poll report stamps every message with a numeric cursor");
    let after_json = cli_poll(&mesh, &joiner.nickname, Some(&first_seq.to_string()));
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
            event["seq"]
                .as_u64()
                .expect("every polled event carries a numeric seq")
                > first_seq,
            "--after must only return events with seq > cursor: {event}"
        );
    }
}

/// The two chat commands over the real Unix socket, through the shipped
/// binary: `a2a broadcast` reaches everyone, `a2a msg` reaches exactly one
/// peer. The in-process suite already proves the routing; this pins the
/// **wire contract** an agent parses — the `type`/`to`/`display` an
/// `--output json` consumer branches on.
#[test]
fn test_cli_broadcast_and_msg_wire_contract() {
    let (creator, mesh) = Node::create();
    let bob = Node::join(&mesh, "wire-bob");
    let carol = Node::join(&mesh, "wire-carol");
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    assert!(bob.wait_ready(&mesh), "bob socket never appeared");
    assert!(carol.wait_ready(&mesh), "carol socket never appeared");
    std::thread::sleep(Duration::from_secs(2));

    let _ = common::cli_message_checked(&mesh, &creator.nickname, "to-everyone");
    // A msg is sealed to its recipient, so its card must have replicated.
    common::await_peer_card_cli(&mesh, &creator.nickname, &bob.nickname);
    let _ = common::cli_msg_checked(&mesh, &creator.nickname, &bob.nickname, "to-bob-only");
    std::thread::sleep(Duration::from_secs(1));

    let events = |nickname: &str| -> Vec<serde_json::Value> {
        serde_json::from_str(&cli_poll(&mesh, nickname, None)).expect("poll returns a JSON array")
    };
    let find = |batch: &[serde_json::Value], body: &str| -> Option<serde_json::Value> {
        batch.iter().find(|event| event["body"] == body).cloned()
    };

    // Broadcast: `type` says everyone, `to` is null, no arrow.
    let bob_events = events(&bob.nickname);
    let broadcast = find(&bob_events, "to-everyone").expect("bob received the broadcast");
    assert_eq!(broadcast["type"], "broadcast", "bob receives a broadcast");
    assert!(broadcast["to"].is_null(), "the broadcast has no recipient");
    assert_eq!(
        broadcast["display"],
        format!("💬️ `<{}>`: to-everyone", creator.nickname),
        "the broadcast display is the expected line"
    );

    // Msg: `type` says one peer, `to` names them, and the arrow marks it.
    let message = find(&bob_events, "to-bob-only").expect("bob received the msg");
    assert_eq!(message["type"], "msg", "bob receives a direct message");
    assert_eq!(
        message["to"], bob.nickname,
        "the direct message is addressed to bob"
    );
    assert_eq!(message["is_visible"], true, "the direct message is visible");
    assert_eq!(
        message["display"],
        format!(
            "💬️ `<{}>` → `<{}>`: to-bob-only",
            creator.nickname, bob.nickname
        ),
        "the direct message display is the expected line"
    );

    // Carol got the broadcast and not the msg — over real sockets, real
    // processes. Asserted on content, not on an empty window.
    let carol_events = events(&carol.nickname);
    assert!(
        find(&carol_events, "to-everyone").is_some(),
        "carol is a live member — she must receive the broadcast"
    );
    assert!(
        find(&carol_events, "to-bob-only").is_none(),
        "a msg must never reach a peer it is not addressed to"
    );
}

/// Poll `nickname` until its stream goes quiet — two consecutive empty batches
/// a beat apart. A bell fires on any waking event past `last_served`, and only a
/// foreground poll advances that cursor, so a bell armed over an undrained
/// convergence backlog (`joined`, `peer_return`) rings on the backlog rather
/// than on whatever the test is about.
fn drain_until_quiet(mesh: &str, nickname: &str) {
    let deadline = Instant::now() + MSG_TIMEOUT;
    let mut quiet = 0;
    while quiet < 2 {
        if cli_poll(mesh, nickname, None) == "[]" {
            quiet += 1;
        } else {
            quiet = 0;
        }
        assert!(
            Instant::now() < deadline,
            "{nickname}'s event stream never went quiet"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// The bell's privacy contract, and the reason it matters: an agent parks a
/// `poll --long` and does nothing until it rings, so ringing it is spending
/// someone's turn. A msg between two other peers must not do that.
///
/// The three gates that make it true are each pinned elsewhere — routing
/// (`transport::send`'s unit tests), sealing and non-surfacing (`seal.rs`) — but
/// nothing pinned the bell itself, which is a further inference: no surfaced
/// event ⇒ no ring entry ⇒ `latest_waking_seq` never moves ⇒ the parked waiter
/// stays parked. A change to `is_visible`/`wakes` could break that link without
/// failing any of them.
///
/// Anti-entropy runs at a test cadence here so several backfill rounds land
/// inside the quiet window: a directed frame re-sent onto gossip would reach
/// carol, and `lifecycle::observe` runs before any addressee gate — it would
/// ring her bell with a `peer_return`/`joined` for the *author* even though the
/// body stays sealed and unsurfaced.
#[test]
fn test_msg_to_one_peer_does_not_ring_a_third_peers_bell() {
    let ae = TEST_BACKFILL_SECS.to_string();
    let flags = [("--backfill-interval-secs", ae.as_str())];
    let (alice, mesh) = Node::create_flags("bell-priv", &flags);
    let bob = Node::join_flags(&mesh, "bellpriv-bob", &flags);
    let carol = Node::join_flags(&mesh, "bellpriv-carol", &flags);
    assert!(alice.wait_ready(&mesh), "alice socket never appeared");
    assert!(bob.wait_ready(&mesh), "bob socket never appeared");
    assert!(carol.wait_ready(&mesh), "carol socket never appeared");

    // A msg is sealed to its recipient, so its card must have replicated.
    common::await_peer_card_cli(&mesh, &alice.nickname, &bob.nickname);
    drain_until_quiet(&mesh, &carol.nickname);

    let mut bell = common::park_bell(&mesh, &carol.nickname);
    let _ = common::cli_msg_checked(&mesh, &alice.nickname, &bob.nickname, "to-bob-only");

    // Long enough for several anti-entropy rounds on top of the bell grace.
    std::thread::sleep(Duration::from_secs(4 * TEST_BACKFILL_SECS));
    common::assert_bell_parked(
        &mut bell,
        "a msg addressed to bob rang a bystander's bell — an agent lost a turn \
         to a message it will never be shown",
    );

    // The negative only means something if the bell was live: a broadcast must
    // ring the very same parked poll, and carry only the public body.
    let _ = cli_message(&mesh, &alice.nickname, "to-everyone");
    let rung = common::wait_bell_output(&mut bell);
    assert!(
        rung.contains("to-everyone"),
        "the bell must still ring on a broadcast: {rung}"
    );
    assert!(
        !rung.contains("to-bob-only"),
        "the msg must not even ride along in the batch: {rung}"
    );
}

/// `a2a call` is the task surface and always needs a peer; chat moved to its
/// own two commands. Without `--to` it must fail rather than quietly
/// broadcasting, which is what the old overload did.
#[test]
fn test_cli_a2a_call_requires_a_peer() {
    let out: std::process::Output = common::test_cmd()
        .args([
            "a2a",
            "call",
            "--gossip",
            "deadbeef",
            "--nickname",
            "calm-otter",
            "--method",
            "SendMessage",
            "--text",
            "orphan",
        ])
        .output()
        .expect("a2a call spawns");
    assert!(
        !out.status.success(),
        "a2a call without --to must fail, not broadcast"
    );
}

/// Baseline a node's poll cursor to "now": a first full poll, then advance
/// past its newest seq — so a long-poll after it sees only new events.
fn poll_cursor(mesh: &str, nickname: &str) -> Option<String> {
    let baseline = cli_poll(mesh, nickname, None);
    let baseline: Vec<serde_json::Value> =
        serde_json::from_str(&baseline).expect("the binary prints valid JSON for this command");
    let seqs = baseline.iter().filter_map(|event| event["seq"].as_u64());
    seqs.max().map(|seq| seq.to_string())
}

/// The wire single-park contract: a raw `{"command":"poll",...,"long":true}`
/// with no new traffic is held for ~the daemon's park cap, then returns
/// exactly `[]`. Sent straight over the Unix socket — the `agent-gossip` client would
/// hide the empty return behind its `--long` re-issue loop.
#[test]
fn test_ipc_poll_long_park_times_out_empty() {
    let (creator, mesh) = Node::create_flags("itest", &[("--longpoll-max-ms", "1000")]);
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    std::thread::sleep(Duration::from_secs(2)); // presence settles

    let after = poll_cursor(&mesh, &creator.nickname);
    let cursor = after.map_or(String::new(), |seq| format!("\"after\":{seq},"));
    let line = format!("{{\"command\":\"poll\",\"mesh\":\"{mesh}\",{cursor}\"long\":true}}");
    let started = Instant::now();
    let response = ipc_raw(&mesh, &creator.nickname, &line);
    let elapsed = started.elapsed();
    assert_eq!(response, "[]", "park elapsed quietly → empty array");
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
    let (creator, mesh) = Node::create();
    let joiner = Node::join(&mesh, "joiner-long");
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    assert!(joiner.wait_ready(&mesh), "joiner socket never appeared");
    std::thread::sleep(Duration::from_secs(2)); // presence settles

    let after = poll_cursor(&mesh, &joiner.nickname);

    // Have the creator send ~400ms into the blocking poll; it must return the
    // message well under the daemon's 60s park cap — proving it woke on
    // traffic rather than spinning to an empty timeout.
    let mesh_for_send = mesh.clone();
    let creator_nick = creator.nickname.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        let _ = cli_message(&mesh_for_send, &creator_nick, "via long-poll");
    });
    let (got, resolve_elapsed) = cli_poll_long(&mesh, &joiner.nickname, after.as_deref());
    sender.join().expect("the sender thread did not panic");
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
    let (creator, mesh) = Node::create_flags("itest", &[("--longpoll-max-ms", "1000")]);
    let joiner = Node::join_flags(&mesh, "joiner-loop", &[("--longpoll-max-ms", "1000")]);
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    assert!(joiner.wait_ready(&mesh), "joiner socket never appeared");
    std::thread::sleep(Duration::from_secs(2)); // presence settles

    let after = poll_cursor(&mesh, &joiner.nickname);

    let mesh_for_send = mesh.clone();
    let creator_nick = creator.nickname.clone();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2500));
        let _ = cli_message(&mesh_for_send, &creator_nick, "past the parks");
    });
    let (got, elapsed) = cli_poll_long(&mesh, &joiner.nickname, after.as_deref());
    sender.join().expect("the sender thread did not panic");
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

/// `agent-gossip ping` is daemon-owned: the transient command arms a round over
/// IPC, the daemon broadcasts a probe, every peer auto-pongs, and the
/// originator emits a `ping_report` on its own output stream listing
/// each responder's RTT. The probe/pong never surface as chat. A short
/// `PING_WINDOW_SECS` keeps the round fast.
#[test]
fn test_ping_reports_peer_rtt() {
    let (creator, mesh) = Node::create_flags("itest", &[("--ping-window-secs", "2")]);
    let joiner = Node::join(&mesh, "ping-joiner");
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    assert!(joiner.wait_ready(&mesh), "joiner socket never appeared");

    // Let the mesh + presence settle so the probe reaches the joiner.
    std::thread::sleep(Duration::from_secs(3));

    // Arm the round on the creator; its report lands on the creator's
    // own stream once the 2s window closes.
    cli_ping(&mesh, &creator.nickname);

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
            .any(|message| message.body == "ping" || message.body == "pong"),
        "ping/pong leaked as chat messages to the joiner"
    );
}

/// An empty mesh (every member, including the creator, has left) is
/// **not** dead: joining it must still succeed. The joiner becomes the
/// rendezvous via `ensure`, and peers that arrive later connect to it.
#[test]
fn test_join_empty_mesh_succeeds_and_reseeds() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // Create a mesh, then kill its only member — the mesh is empty.
    let (creator, mesh) = Node::create();
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    creator.sigint();
    drop(creator);
    std::thread::sleep(Duration::from_secs(2));

    // Joining the empty mesh must succeed (previously this hung until
    // a 30s timeout and exited non-zero).
    let first = Node::join(&mesh, "empty-joiner");
    assert!(
        first.wait_ready(&mesh),
        "joining an empty mesh should succeed, not hang\nlog tail:\n{}",
        first.log_tail(15),
    );

    // A peer arriving later must be able to connect to that joiner,
    // which is now the rendezvous.
    let second = Node::join(&mesh, "later-peer");
    assert!(
        second.wait_ready(&mesh),
        "later peer could not connect to the re-seeded mesh\nfirst log:\n{}\nsecond log:\n{}",
        first.log_tail(15),
        second.log_tail(15),
    );

    // And messages flow across the re-seeded mesh.
    let id = cli_message(&mesh, &first.nickname, "re-seeded hello");
    assert!(!id.is_empty(), "msg returned empty id");
    let total = wait_until(
        || first.messages().count() + second.messages().count(),
        1,
        MSG_TIMEOUT,
    );
    assert!(
        total >= 1,
        "no message crossed the re-seeded mesh\nfirst: {}\nsecond: {}",
        chat_debug(&first),
        chat_debug(&second),
    );
}

/// The reply of the `topology` query of the running daemon, as JSON.
/// `node` names the daemon that is asked, for the failure text.
fn topology_reply(mesh: &str, nickname: &str, node: &str) -> serde_json::Value {
    let out: std::process::Output = common::test_cmd()
        .args(["topology", "--gossip", mesh, "--nickname", nickname])
        .output()
        .expect("failed to run topology");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "topology of the {node} ({nickname}) failed: status {:?}\nstdout: {stdout}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!(
            "topology of the {node} ({nickname}) printed no JSON object: {error}\nstdout: {stdout}"
        )
    })
}

/// The `self_id` that the running daemon's `topology` reports. The daemon
/// reads it from its multihop handle, so it is empty when the session has none.
fn multihop_self_id(mesh: &str, nickname: &str, node: &str) -> String {
    let reply = topology_reply(mesh, nickname, node);
    reply["topology"]["self_id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("topology of the {node} ({nickname}) has no self_id\nstdout: {reply}")
        })
        .to_owned()
}

/// `--max-connections` reaches the engine, and the `topology` reply reports the
/// cap, its use, and a link to the joined peer. The cap is 8, the lowest the engine accepts and
/// not the default 16, so the reply tells the flag from the default.
#[test]
fn test_topology_reports_the_cap_and_the_links() {
    let _serial = serial_guard();
    let (creator, mesh) = Node::create_args("topology-cap", &["--max-connections", "8"], &[]);
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    let joiner = Node::join(&mesh, "cap-joiner");
    assert!(
        joiner.wait_ready(&mesh),
        "joiner socket never appeared\nlog tail:\n{}",
        joiner.log_tail(15),
    );
    let mut reply = topology_reply(&mesh, &creator.nickname, "creator");
    for _ in 0..40 {
        if reply["links"]
            .as_array()
            .is_some_and(|links| !links.is_empty())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        reply = topology_reply(&mesh, &creator.nickname, "creator");
    }
    assert_eq!(
        reply["cap"]["max_connections"], 8,
        "the flag reaches the engine\n{reply}"
    );
    assert_eq!(
        reply["cap"]["connected_cap"], 16,
        "the total cap is twice the flag\n{reply}"
    );
    let links = reply["links"]
        .as_array()
        .unwrap_or_else(|| panic!("the daemon sent no links\n{reply}"));
    assert!(
        links
            .iter()
            .any(|link| link["nickname"] == joiner.nickname.as_str()),
        "the creator holds a link to the joiner\n{reply}"
    );
}

/// `topology --draw` against a live daemon joins the roster to the endpoints of
/// the topology reply. Without the `endpoints` key of that reply no peer takes
/// the flags of its link, so the drawing shows the neighbor flag of a joined
/// peer only when the key is there.
#[test]
fn test_topology_draw_shows_the_link_flags_of_a_joined_peer() {
    let _serial = serial_guard();
    let (creator, mesh) = Node::create();
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    let joiner = Node::join(&mesh, "draw-joiner");
    assert!(
        joiner.wait_ready(&mesh),
        "joiner socket never appeared\nlog tail:\n{}",
        joiner.log_tail(15),
    );
    let linked_and_named = || {
        let reply = topology_reply(&mesh, &creator.nickname, "creator");
        let links = reply["links"].as_array().cloned().unwrap_or_default();
        let endpoints = reply["endpoints"].as_array().cloned().unwrap_or_default();
        let linked = links.iter().any(|link| link["neighbor"] == true);
        let named = endpoints
            .iter()
            .any(|entry| entry["nickname"] == joiner.nickname.as_str());
        usize::from(linked && named)
    };
    assert_eq!(
        wait_until(linked_and_named, 1, MSG_TIMEOUT),
        1,
        "the creator never held a gossip link to the joiner and learned its endpoint"
    );

    let out = common::test_cmd()
        .args(["topology", "--draw", "--gossip", &mesh])
        .args(["--nickname", &creator.nickname])
        .output()
        .expect("failed to run topology --draw");

    let drawing = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "topology --draw failed: {:?}\nstdout: {drawing}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        drawing.contains(&joiner.nickname),
        "the drawing names the joiner\n{drawing}"
    );
    assert!(
        drawing.contains("neighbor"),
        "the joiner takes the neighbor flag of its link, through the endpoints of the reply\n{drawing}"
    );
}

/// The endpoint ids that the link-state graph of a `topology` reply reaches from its own node,
/// that node included.
fn reachable_endpoints(reply: &serde_json::Value) -> HashSet<String> {
    let start = reply["topology"]["self_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let edges = reply["topology"]["edges"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut seen: HashSet<String> = HashSet::from([start.clone()]);
    let mut frontier = vec![start];
    while let Some(node) = frontier.pop() {
        for edge in edges.iter().filter(|edge| edge["from"] == node.as_str()) {
            if let Some(next) = edge["to"].as_str()
                && seen.insert(next.to_owned())
            {
                frontier.push(next.to_owned());
            }
        }
    }
    seen
}

/// The headline of the multihop transport: at `--max-connections 8`, the lowest cap the engine
/// accepts, a node of ten cannot hold a direct connection to each of the nine others, and every
/// directed message still arrives. A peer past the cap is pinned: the `topology` reply names its
/// first hop.
#[test]
fn test_directed_messages_cross_a_cap_of_eight_and_the_topology_pins_the_far_peers() {
    const CAP8: [(&str, &str); 1] = [("--max-connections", "8")];
    let _serial = serial_guard();
    let (creator, mesh) = Node::create_flags("cap-eight", &CAP8);
    let joiners: Vec<Node> = (0..9)
        .map(|index| Node::join_flags(&mesh, &format!("cp-{index}"), &CAP8))
        .collect();
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    for joiner in &joiners {
        assert!(joiner.wait_ready(&mesh), "{} never ready", joiner.nickname);
    }
    let all: Vec<&Node> = std::iter::once(&creator).chain(joiners.iter()).collect();
    // A send past the cap takes a multihop route only when the node knows one. Before that it dials
    // direct and holds an extra connection, which is not pinned. Every directed call below,
    // the card probes included, waits for the link-state graphs to reach all nodes.
    let nodes_that_reach_all = || {
        let reaching = all.iter().filter(|node| {
            let reply = topology_reply(&mesh, &node.nickname, &node.nickname);
            reachable_endpoints(&reply).len() == all.len()
        });
        reaching.count()
    };
    let reaching = wait_until(nodes_that_reach_all, all.len(), RECOVERY_TIMEOUT);
    let graph_of_each_node: Vec<String> = all
        .iter()
        .map(|node| {
            let reply = topology_reply(&mesh, &node.nickname, &node.nickname);
            format!(
                "{}: self {} reaches {} edges {} links {}",
                node.nickname,
                reply["topology"]["self_id"],
                reachable_endpoints(&reply).len(),
                reply["topology"]["edges"],
                reply["links"]
            )
        })
        .collect();
    assert_eq!(
        reaching,
        all.len(),
        "the link-state graph of a node never reached all {} nodes\n{}",
        all.len(),
        graph_of_each_node.join("\n")
    );
    for sender in &all {
        for receiver in &all {
            if sender.nickname != receiver.nickname {
                common::await_peer_card_cli(&mesh, &sender.nickname, &receiver.nickname);
            }
        }
    }

    for sender in &all {
        for receiver in &all {
            if sender.nickname == receiver.nickname {
                continue;
            }
            let body = format!("{}-to-{}", sender.nickname, receiver.nickname);
            let _ = common::cli_msg_checked(&mesh, &sender.nickname, &receiver.nickname, &body);
            assert_received(receiver, &sender.nickname, &body, RECOVERY_TIMEOUT);
        }
    }

    let pinned_with_hop = || {
        let mut count = 0;
        for node in &all {
            let reply = topology_reply(&mesh, &node.nickname, &node.nickname);
            let links = reply["links"].as_array().cloned().unwrap_or_default();
            let pins = links
                .iter()
                .any(|link| link["pinned"] == true && link["via"].is_string());
            count += usize::from(pins);
        }
        count
    };
    let pinned_seen = wait_until(pinned_with_hop, 1, MSG_TIMEOUT);
    let state_of_each_node: Vec<String> = all
        .iter()
        .map(|node| {
            let reply = topology_reply(&mesh, &node.nickname, &node.nickname);
            format!(
                "{}: cap {} links {}",
                node.nickname, reply["cap"], reply["links"]
            )
        })
        .collect();
    assert!(
        pinned_seen >= 1,
        "no node reports a pinned link with a first hop at a cap of 8\n{}",
        state_of_each_node.join("\n")
    );
}

/// Create a mesh with `create_args`, join it, and assert which sessions hold a
/// multihop handle. The joiner takes no transport flag: its policy can come
/// only from the mesh id.
fn assert_joiner_multihop(name: &str, create_args: &[&str], expect_multihop: bool) {
    let _serial = serial_guard();
    let (creator, mesh) = Node::create_args(name, create_args, &[]);
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    let joiner = Node::join(&mesh, "policy-joiner");
    assert!(
        joiner.wait_ready(&mesh),
        "joiner socket never appeared\nlog tail:\n{}",
        joiner.log_tail(15),
    );
    // The creator is the control: it proves `topology` tells the two policies apart.
    let creator_id = multihop_self_id(&mesh, &creator.nickname, "creator");
    assert_eq!(
        !creator_id.is_empty(),
        expect_multihop,
        "creator multihop handle, self_id {creator_id:?}"
    );
    let joiner_id = multihop_self_id(&mesh, &joiner.nickname, "joiner");
    assert_eq!(
        !joiner_id.is_empty(),
        expect_multihop,
        "the joiner must take its transport policy from the mesh id, self_id {joiner_id:?}"
    );
}

/// A mesh id made with `udp,webrtc` names no multihop, so its joiner gets no
/// multihop handle although the app default has one.
#[test]
fn test_joiner_takes_no_multihop_from_a_mesh_id_that_names_none() {
    assert_joiner_multihop("join-policy-nomh", &["--transport", "udp,webrtc"], false);
}

/// A mesh id made with the app default (`udp,webrtc,multihop`) gives its joiner
/// a multihop handle.
#[test]
fn test_joiner_takes_multihop_from_the_app_default_id() {
    assert_joiner_multihop("join-policy-default", &[], true);
}

/// The headline resilience guarantee: a mesh survives its creator's death
/// as long as any member is still up. Creator + bystander form a mesh; the
/// creator is hard-killed; a brand-new joiner must still bootstrap — proof
/// that the bystander took over the seed-derived rendezvous (private mode:
/// claim-if-free on the deterministic loopback port).
#[test]
fn test_join_after_creator_departed_with_surviving_member() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // 1. Creator + a bystander that will outlive it. Heal-only profile:
    //    the handoff is heal-gated, and production evict windows keep the
    //    test's membership semantics unchanged.
    let (mut creator, mesh) = Node::create_flags("itest", &FAST_HEAL);
    let bystander = Node::join_flags(&mesh, "bystander", &FAST_HEAL);
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    assert!(
        bystander.wait_ready(&mesh),
        "bystander socket never appeared"
    );

    // Confirm the mesh is actually live before we kill the creator.
    let pre_id = cli_message(&mesh, &creator.nickname, "pre-death ping");
    assert!(!pre_id.is_empty(), "msg returned empty id");
    let delivered = wait_until(
        || creator.messages().count() + bystander.messages().count(),
        1,
        MSG_TIMEOUT,
    );
    assert!(delivered >= 1, "creator/bystander never meshed pre-death");

    // 2. Retire the creator and wait for its process to fully exit —
    //    graceful `Left` + clean close + socket release — then poll
    //    until the bystander serves the rendezvous. Joining while the
    //    old socket lingers or before a survivor beacon is bridged is
    //    the historical flake (see `survivor_serves_rendezvous`).
    creator.sigint();
    creator.wait_exit();
    drop(creator);
    assert!(
        wait_rendezvous_served(&mesh, &[&bystander.nickname]),
        "bystander never served the rendezvous after creator exit\n{}\nbystander log tail:\n{}",
        rendezvous_diagnosis(&mesh, &bystander.nickname),
        bystander.log_tail(15),
    );

    // 3. A brand-new joiner that never saw the creator. Its only
    //    bootstrap target is the seed-derived rendezvous id; reaching
    //    `ready` proves the bystander is now serving it.
    let latecomer = Node::join_flags(&mesh, "latecomer", &FAST_HEAL);
    assert!(
        latecomer.wait_ready(&mesh),
        "latecomer could not join after creator death — rendezvous failover broke\nbystander log tail:\n{}\nlatecomer log tail:\n{}",
        bystander.log_tail(15),
        latecomer.log_tail(15),
    );

    // 4. And the mesh genuinely works end-to-end across the failover:
    //    a message from the latecomer reaches the bystander.
    let post_id = cli_message(&mesh, &latecomer.nickname, "post-death hello");
    assert!(!post_id.is_empty(), "latecomer msg returned empty id");
    let total = wait_until(
        || bystander.messages().count() + latecomer.messages().count(),
        1,
        MSG_TIMEOUT,
    );
    assert!(
        total >= 1,
        "latecomer joined but no message crossed the post-failover mesh\nbystander msgs: {}",
        chat_debug(&bystander),
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
    // Production heal cadence — see `RENDEZVOUS_HANDOFF` for why this
    // test must not inject a short one.
    let (creator, mesh) = Node::create();
    let bystander = Node::join(&mesh, "fm-bystander");
    assert!(creator.wait_ready(&mesh), "creator socket never appeared");
    assert!(
        bystander.wait_ready(&mesh),
        "bystander socket never appeared"
    );
    // Confirm the mesh is live before killing the creator.
    let _ = cli_message(&mesh, &creator.nickname, "warmup");
    assert!(
        wait_until(
            || creator.messages().count() + bystander.messages().count(),
            1,
            MSG_TIMEOUT
        ) >= 1,
        "creator/bystander never meshed pre-death"
    );

    // This test exercises post-departure-join *delivery*, not migration
    // speed, so the migration must be fully settled first — the blind
    // migration wait; see `RENDEZVOUS_HANDOFF` for why this cannot be a
    // marker poll. `drop` SIGKILLs before the graceful grace completes:
    // deliberate — letting the shutdown finish makes the bystander's
    // fast-reclaim re-stand the beacon while its gossip still holds the
    // dying beacon's link (same endpoint id), tripping the iroh
    // stale-connection stall (iroh-gossip#10) instead of a clean
    // claim-on-heal-tick.
    creator.sigint();
    drop(creator);
    std::thread::sleep(RENDEZVOUS_HANDOFF);

    let joiner = Node::join(&mesh, "fm-joiner");
    assert!(
        joiner.wait_ready(&mesh),
        "joiner could not join after creator death\nbystander:\n{}\njoiner:\n{}",
        bystander.log_tail(15),
        joiner.log_tail(15),
    );

    // First broadcast, sent immediately after `ready` (the exact bug
    // trigger): joiner -> bystander. Post-disruption delivery, so it gets
    // `RECOVERY_TIMEOUT`, not the steady-state `MSG_TIMEOUT`: routing the
    // joiner's first message waits on its `NeighborUp` re-announce, which is
    // gated by the heal cadence, so a join that just missed a heal tick
    // legitimately needs another cycle.
    let j2b_id = cli_message(&mesh, &joiner.nickname, "j2b first");
    assert!(!j2b_id.is_empty(), "joiner msg returned empty id");
    assert!(
        wait_until(
            || bystander.count_from(&joiner.nickname, "j2b first"),
            1,
            RECOVERY_TIMEOUT
        ) >= 1,
        "joiner's first message was NOT received by the existing peer\nbystander msgs: {}",
        chat_debug(&bystander),
    );

    // Reverse direction: existing peer -> joiner. Fails if the joiner
    // was never integrated into the mesh (lost first PeerInfo, no
    // re-announce).
    let b2j_id = cli_message(&mesh, &bystander.nickname, "b2j first");
    assert!(!b2j_id.is_empty(), "bystander msg returned empty id");
    assert!(
        wait_until(
            || joiner.count_from(&bystander.nickname, "b2j first"),
            1,
            RECOVERY_TIMEOUT
        ) >= 1,
        "existing peer's message was NOT received by the joiner\njoiner msgs: {}",
        chat_debug(&joiner),
    );
}

/// Join horizon: a peer that joins after history was taskd must
/// **not surface** that pre-join history (anti-entropy still relays it
/// at the wire for mesh-wide resilience — that is intentionally not
/// observable here; only the view is filtered). A message sent *after*
/// it joined must still arrive, proving the node is meshed and only
/// the horizon, not connectivity, hides the old messages.
///
/// Subprocess (not `InProcNode`): the negative wait must span several
/// anti-entropy cycles, and only a spawned daemon can take the hidden
/// `--backfill-interval-secs` flag (the api path runs production
/// defaults).
#[test]
fn test_join_horizon_hides_pre_join_history() {
    let (creator, mesh) = Node::create_flags("nethorizon", &FAST_AE);
    let early = Node::join_flags(&mesh, "jh-early", &FAST_AE);
    assert!(
        creator.wait_ready(&mesh),
        "the creator daemon must bind its control socket before the test sends it any command"
    );
    assert!(early.wait_ready(&mesh), "early peer never ready");

    // History taskd *before* the late peer exists.
    for tag in ["hist-1", "hist-2", "hist-3"] {
        let _ = cli_message(&mesh, &creator.nickname, tag);
        assert!(
            wait_until(|| early.count_from(&creator.nickname, tag), 1, MSG_TIMEOUT) >= 1,
            "history not delivered to the existing peer before the late join: {tag}"
        );
    }

    // Whole-second timestamps: keep the history strictly an earlier
    // second than the late join (off the 1-second boundary; the real
    // case is seconds-to-minutes old).
    std::thread::sleep(Duration::from_secs(2));

    let late = Node::join_flags(&mesh, "jh-late", &FAST_AE);
    assert!(late.wait_ready(&mesh), "late peer never ready");

    // Well over an anti-entropy cycle (at the injected cadence) so a
    // later "still zero" means the horizon suppressed the backfill, not
    // that it merely hadn't arrived yet. Inherent blind wait (negative
    // assertion).
    std::thread::sleep(Duration::from_secs(3 * TEST_BACKFILL_SECS));
    for tag in ["hist-1", "hist-2", "hist-3"] {
        assert_eq!(
            late.count_from(&creator.nickname, tag),
            0,
            "pre-join history {tag} was surfaced to the late joiner"
        );
    }

    // But it IS meshed: a message sent after it joined must surface.
    let _ = cli_message(&mesh, &creator.nickname, "post-join-live");
    assert!(
        wait_until(
            || late.count_from(&creator.nickname, "post-join-live"),
            1,
            MSG_TIMEOUT
        ) >= 1,
        "post-join message not delivered to the late joiner — connectivity broken, not just horizon"
    );
}

// ── reliability tests ────────────────────────────────────────────────────────
//
// Heal cadence only — for tests whose waits are heal-gated but whose
// semantics need the production eviction windows.
const FAST_HEAL: [(&str, &str); 1] = [("--heal-interval-secs", "3")];

// Short eviction at the production heal cadence — for the migration
// tests that must not shorten the heal interval (see
// `RENDEZVOUS_HANDOFF`).
const EVICT_ONLY: [(&str, &str); 2] = [
    ("--alive-timeout-secs", "3"),
    ("--sweep-interval-secs", "1"),
];

// Fast reconcile: short heal + anti-entropy cadences with production
// eviction windows — for the backfill tests, whose frozen peer must
// STAY a member (no `SHORT_EVICT`) while recovery converges quickly.
const FAST_AE: [(&str, &str); 2] = [
    ("--heal-interval-secs", "3"),
    ("--backfill-interval-secs", "2"),
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

    // `EVICT_ONLY`, not `SHORT_EVICT`: this migration must run at the
    // production heal cadence — see `RENDEZVOUS_HANDOFF`.
    let (creator, mesh) = Node::create_flags("itest", &EVICT_ONLY);
    let alpha = Node::join_flags(&mesh, "ck-alpha", &EVICT_ONLY);
    let bravo = Node::join_flags(&mesh, "ck-bravo", &EVICT_ONLY);
    assert!(
        creator.wait_ready(&mesh),
        "the creator daemon must bind its control socket before the test sends it any command"
    );
    assert!(
        alpha.wait_ready(&mesh),
        "the alpha daemon must bind its control socket before the test sends it any command"
    );
    assert!(
        bravo.wait_ready(&mesh),
        "the bravo daemon must bind its control socket before the test sends it any command"
    );
    let _ = cli_message(&mesh, &creator.nickname, "ck-base");
    assert_received(&alpha, &creator.nickname, "ck-base", MSG_TIMEOUT);
    assert_received(&bravo, &creator.nickname, "ck-base", MSG_TIMEOUT);

    // Silent vanish: peers learn only via the alive-timeout path.
    creator.kill();
    drop(creator);
    let _ = cli_message(&mesh, &alpha.nickname, "ck-survive");
    assert_received(&bravo, &alpha.nickname, "ck-survive", RECOVERY_TIMEOUT);

    // A brand-new joiner can only reach the mesh if a survivor now
    // serves the seed-derived rendezvous. Blind migration wait — see
    // `RENDEZVOUS_HANDOFF` for why this cannot be a marker poll.
    std::thread::sleep(RENDEZVOUS_HANDOFF);
    let charlie = Node::join_flags(&mesh, "ck-charlie", &EVICT_ONLY);
    assert!(
        charlie.wait_ready(&mesh),
        "fresh joiner could not bootstrap after creator SIGKILL\nalpha:\n{}\ncharlie:\n{}",
        alpha.log_tail(15),
        charlie.log_tail(20),
    );
    let _ = cli_message(&mesh, &charlie.nickname, "ck-newcomer");
    assert_received(&alpha, &charlie.nickname, "ck-newcomer", RECOVERY_TIMEOUT);
}

/// Orphan self-termination: when the agent that spawned the daemon is
/// hard-killed, the daemon is reparented (not killed) and would otherwise
/// linger in the mesh forever. The `getppid` watcher must notice the
/// reparent and exit through the graceful `left`-broadcasting path.
///
/// To orphan the daemon without killing the test, we launch it under a
/// throwaway shell that backgrounds it and then `exec`s `sleep` to stay
/// alive as its parent. Killing that shell (not the daemon) is what an
/// agent reinstall / `kill -9` does in production.
#[test]
fn test_orphaned_daemon_self_terminates() {
    let _serial = serial_guard();

    let (observer, mesh) = Node::create_named("itest-orphan");
    assert!(
        observer.wait_ready(&mesh),
        "the observer daemon must bind its control socket before the test sends it any command"
    );

    // Background the joiner under a shell, record its pid, then `exec sleep`
    // so the shell's pid *is* the joiner's parent for the rest of its life.
    // A 200ms watch interval means orphaning is noticed in well under a second.
    let pid_file = tmp_log("orphan-pid");
    let joiner_log = tmp_log("orphan-joiner-out");
    let script = format!(
        "'{bin}' --log-dir '{dir}' join {mesh} --nickname orphan-joiner \
            --ppid-watch-interval-ms 200 >'{out}' 2>&1 & \
         echo $! >'{pid}'; exec sleep 600",
        bin = bin().display(),
        dir = common::test_log_dir(),
        mesh = mesh,
        out = joiner_log.display(),
        pid = pid_file.display(),
    );
    // `Child` has no kill-on-drop, so guard the intermediate
    // shell: if an assertion panics before the explicit kill below, this drop
    // still reaps it (which orphans the daemon → the watcher terminates it).
    let mut parent = KillOnDrop::new(
        Command::new("sh")
            .arg("-c")
            .arg(&script)
            .spawn()
            .expect("failed to spawn the intermediate parent shell"),
    );

    // Wait until the joiner is meshed: its socket exists and the observer's
    // roster lists it. Only then does its graceful `left` have a live link.
    let joiner_sock = socket_path(&mesh, "orphan-joiner");
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline
        && !(std::path::Path::new(&joiner_sock).exists()
            && cli_peers(&mesh, &observer.nickname).contains("orphan-joiner"))
    {
        std::thread::sleep(POLL);
    }
    assert!(
        cli_peers(&mesh, &observer.nickname).contains("orphan-joiner"),
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
        .args(["-KILL", &parent.id().to_string()])
        .status();
    let _ = parent.wait();

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
        && cli_peers(&mesh, &observer.nickname).contains("orphan-joiner")
    {
        std::thread::sleep(POLL);
    }
    assert!(
        !cli_peers(&mesh, &observer.nickname).contains("orphan-joiner"),
        "observer never received the orphaned daemon's graceful 'left'\nobserver:\n{}",
        observer.log_tail(15),
    );

    let _ = fs::remove_file(&pid_file);
    let _ = fs::remove_file(&joiner_log);
}

/// Fixed-node-id reconnect must be *fast*. A `SIGSTOP`/`SIGCONT` peer
/// resumes in the **same** process with the **same** iroh endpoint id —
/// exactly the iroh-gossip#10 / psyche #25 / p2panda #695 hazard, where a
/// stale *accepted* connection on a reused id can force a multi-minute
/// timeout before the peer is re-admitted to the gossip overlay. Our
/// heal + resume re-bootstrap must sidestep that, so we probe the woken
/// peer *immediately* on `SIGCONT` and require delivery within a bound
/// comfortably below that stale-connection timeout. A regression (or an
/// iroh bump reintroducing the stall) blows the bound. A pass proves that
/// recovery happens and that it is heal-bound, not timeout-bound.
#[test]
fn test_fixed_id_reconnect_admits_fast() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // Eviction lands after ALIVE_TIMEOUT + SWEEP_INTERVAL (3+1s); the
    // ceiling is slack for a loaded host, not paid time.
    let evict_bound = Duration::from_secs(12);
    // Above our re-mesh cost (a few heal cycles at the injected cadence
    // + resume re-bootstrap + anti-entropy backfill) yet far below
    // iroh's minutes-long stale-connection timeout: a pass means
    // admission is heal-bound.
    let admit_bound = Duration::from_secs((5 * TEST_HEAL_SECS + 10).max(15));

    let (creator, mesh) = Node::create_flags("itest", &SHORT_EVICT);
    let sleeper = Node::join_flags(&mesh, "fr-sleeper", &SHORT_EVICT);
    assert!(
        creator.wait_ready(&mesh),
        "the creator daemon must bind its control socket before the test sends it any command"
    );
    assert!(
        sleeper.wait_ready(&mesh),
        "the sleeper daemon must bind its control socket before the test sends it any command"
    );
    let _ = cli_message(&mesh, &creator.nickname, "fr-pre");
    assert_received(&sleeper, &creator.nickname, "fr-pre", MSG_TIMEOUT);

    sleeper.stop();
    assert!(
        wait_until(
            || usize::from(creator.log_contents().contains("went quiet")),
            1,
            evict_bound,
        ) >= 1,
        "creator never evicted the frozen peer\n{}",
        creator.log_tail(20),
    );

    // Resume, then probe at once: the message is sent while the woken
    // peer is reconnecting with its original id. Receipt within the
    // bound proves fast re-admission, not a stale-connection stall.
    sleeper.cont();
    let _ = cli_message(&mesh, &creator.nickname, "fr-post");
    assert_received(&sleeper, &creator.nickname, "fr-post", admit_bound);
}

/// Resume hard re-bootstrap: a peer frozen *past the stall threshold*
/// (process throttle / sleep proxy) must take the hard recovery path
/// — reset `meshed`, re-assert the rendezvous hint, long probe — not
/// just the weak periodic heal that the old code relied on (and which
/// silently failed to rebuild a fully-collapsed mesh). Shortens
/// `--heal-stall-threshold-secs` so a 12s `SIGSTOP` trips it, then
/// asserts both the hard-path log marker and that post-wake traffic
/// flows again.
#[test]
fn test_resume_triggers_hard_rebootstrap() {
    // SHORT_EVICT + a shortened stall threshold. The threshold MUST
    // comfortably exceed the injected heal cadence (else every normal
    // heal tick false-positives as a resume and the node hard-reboots
    // forever), and the freeze MUST exceed the threshold. 3s cadence <
    // 8s threshold < 12s freeze satisfies both; production is 15s/60s.
    const STALL_EVICT: [(&str, &str); 4] = [
        ("--alive-timeout-secs", "3"),
        ("--sweep-interval-secs", "1"),
        ("--heal-interval-secs", "3"),
        ("--heal-stall-threshold-secs", "8"),
    ];
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // > stall threshold (8s), > evict window (3+1s).
    let asleep = Duration::from_secs(12);
    // tokio burst-fires the missed heal tick on SIGCONT; ceiling for the
    // hard path to run and log its markers (adaptive — polled below).
    let wake_settle = Duration::from_secs(4 * TEST_HEAL_SECS + 8);

    let (creator, mesh) = Node::create_flags("itest", &STALL_EVICT);
    let sleeper = Node::join_flags(&mesh, "rb-sleeper", &STALL_EVICT);
    assert!(
        creator.wait_ready(&mesh),
        "the creator daemon must bind its control socket before the test sends it any command"
    );
    assert!(
        sleeper.wait_ready(&mesh),
        "the sleeper daemon must bind its control socket before the test sends it any command"
    );
    let _ = cli_message(&mesh, &creator.nickname, "rb-pre");
    assert_received(&sleeper, &creator.nickname, "rb-pre", MSG_TIMEOUT);

    sleeper.stop();
    std::thread::sleep(asleep);
    sleeper.cont();

    // The hard-path markers are `tracing` output — they land in the
    // sink log (`--log-dir`), not the operator stdout/stderr capture.
    // "hard re-bootstrap edge" is the hard path itself;
    // "rendezvous-independent re-bridge" proves the resume also re-dials
    // known peers directly rather than relying solely on a rendezvous
    // graft that a stale connection (iroh-gossip#10) could stall.
    let both_markers = || {
        let trace = trace_log(&mesh, &sleeper.nickname);
        usize::from(
            trace.contains("hard re-bootstrap edge")
                && trace.contains("rendezvous-independent re-bridge"),
        )
    };
    let reached = wait_until(both_markers, 1, wake_settle) >= 1;
    let sink = trace_log(&mesh, &sleeper.nickname);
    let mut sink_tail: Vec<&str> = sink.lines().rev().take(30).collect();
    sink_tail.reverse();
    assert!(
        reached,
        "woken peer never took the hard re-bootstrap path (or skipped the re-bridge)\nsink tail:\n{}",
        sink_tail.join("\n"),
    );
    let _ = cli_message(&mesh, &creator.nickname, "rb-post");
    assert_received(&sleeper, &creator.nickname, "rb-post", RECOVERY_TIMEOUT);
}

/// A directed frame is unicast-only on the send path, and no resend may be the hole in that: a
/// repair that put the frame on the gossip flood would reach a bystander, which
/// `transport::deliver` structurally keeps off it.
///
/// The oracle is the receiver's *wire*, not its screen: the engine's redaction
/// log records every ingested frame at `recv::ingest` — before any addressee
/// gate — so a `directed` line naming bob in carol's log means the bytes
/// reached her, whether or not she could read or surface them. That is the
/// leak, and it is also how a private msg would ring her bell:
/// `lifecycle::observe` runs on the frame first and can emit a waking
/// `peer_return`/`joined` for its author.
#[test]
fn test_directed_msg_never_reaches_a_bystander() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();

    let (alice, mesh) = Node::create_flags("itest", &FAST_AE);
    let bob = Node::join_flags(&mesh, "leak-bob", &FAST_AE);
    let carol = Node::join_flags(&mesh, "leak-carol", &FAST_AE);
    assert!(
        alice.wait_ready(&mesh),
        "the alice daemon must bind its control socket before the test sends it any command"
    );
    assert!(
        bob.wait_ready(&mesh),
        "the bob daemon must bind its control socket before the test sends it any command"
    );
    assert!(
        carol.wait_ready(&mesh),
        "the carol daemon must bind its control socket before the test sends it any command"
    );

    // A msg is sealed to its recipient, so its card must have replicated.
    common::await_peer_card_cli(&mesh, &alice.nickname, &bob.nickname);
    let _ = common::cli_msg_checked(&mesh, &alice.nickname, &bob.nickname, "leak-secret");
    assert_received(&bob, &alice.nickname, "leak-secret", MSG_TIMEOUT);

    // Wait out several backfill rounds. Inherent blind wait (negative assertion), derived from
    // the injected cadence.
    std::thread::sleep(Duration::from_secs(5 * TEST_BACKFILL_SECS));

    // NOTE: greps the `directed` line emitted by `logging::messages::log`.
    // Keep the two in sync — if that event name is reworded, update this match.
    let trace = trace_log(&mesh, &carol.nickname);
    let matching = trace
        .lines()
        .filter(|line| line.contains("directed") && line.contains(&bob.nickname));
    let leaked: Vec<String> = matching.map(str::to_owned).collect();
    assert!(
        leaked.is_empty(),
        "a frame directed at bob reached a bystander's wire:\n{}",
        leaked.join("\n")
    );
}

/// A frozen peer catches up on the state and the meta channel after the thaw. The engine keeps no
/// log of chat, so only the slices of these two channels are synced by the backfill round: the
/// gossip layer holds each frame for 30 s, and the freeze lasts longer, so a slice written during
/// the freeze reaches the peer only through the backfill. The test does not assert that chat is
/// missed: a buffered stream can still deliver it after the thaw.
///
/// Not `SHORT_EVICT`: the peer must stay a member, so the production alive-timeout is required.
#[test]
fn test_a_frozen_peer_catches_up_on_state_and_meta_after_the_thaw() {
    // Longer than the 30 s that the gossip layer holds a frame.
    const FREEZE: Duration = Duration::from_secs(35);
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    let backfill = TEST_BACKFILL_SECS.to_string();
    let envs = [
        ("--heal-interval-secs", "3"),
        ("--backfill-interval-secs", backfill.as_str()),
    ];

    let (creator, mesh) = Node::create_args("itest", &[], &envs);
    let alpha = Node::join_flags(&mesh, "fz-alpha", &envs);
    let bravo = Node::join_flags(&mesh, "fz-bravo", &envs);
    assert!(creator.wait_ready(&mesh), "the creator never served");
    assert!(alpha.wait_ready(&mesh), "alpha never served");
    assert!(bravo.wait_ready(&mesh), "bravo never served");
    common::await_link_cli(&mesh, &creator.nickname, &alpha.nickname);
    common::await_link_cli(&mesh, &creator.nickname, &bravo.nickname);

    alpha.stop();
    for (channel, merge) in [
        (agent_gossip::Channel::State, r#"{"frozen":"state"}"#),
        (agent_gossip::Channel::Meta, r#"{"frozen":"meta"}"#),
    ] {
        let out = common::cli_channel_merge(channel, &mesh, &creator.nickname, merge);
        assert!(
            out.status.success(),
            "the merge during the freeze failed: {out:?}"
        );
    }
    // bravo is live: it gets both writes, so they did go out during the freeze.
    for (channel, value) in [
        (agent_gossip::Channel::State, "state"),
        (agent_gossip::Channel::Meta, "meta"),
    ] {
        assert!(
            await_slice_value((&mesh, &bravo.nickname), channel, value, MSG_TIMEOUT),
            "bravo never got the {value} write"
        );
    }
    std::thread::sleep(FREEZE);
    alpha.cont();

    // The backfill round at the injected cadence, plus a heal if the freeze dropped the link.
    let catch_up = Duration::from_secs(10 * TEST_BACKFILL_SECS + 6 * TEST_HEAL_SECS);
    for (channel, value) in [
        (agent_gossip::Channel::State, "state"),
        (agent_gossip::Channel::Meta, "meta"),
    ] {
        assert!(
            await_slice_value((&mesh, &alpha.nickname), channel, value, catch_up),
            "alpha never caught up on the {value} write after the thaw\n{}",
            alpha.log_tail(20)
        );
    }
}

/// Whether the view of `channel` at `nickname` holds a slice with `"frozen": value`, within `limit`.
fn await_slice_value(
    (mesh, nickname): (&str, &str),
    channel: agent_gossip::Channel,
    value: &str,
    limit: Duration,
) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        let raw = common::cli_channel_get(channel, mesh, nickname);
        let view: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
        let held = view["document"]
            .as_object()
            .is_some_and(|slices| slices.values().any(|slice| slice["frozen"] == value));
        if held {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

// ── Key-identity model (Tier 1) ─────────────────────────────────────────

/// BEFORE (`same_nickname_peers_communicate`, from the key-identity work in main, #47): two members share
/// a display nickname, and each must still see the other's messages, because self-echo is keyed on the key
/// and not on the name. AFTER: the nickname gate admits one key for a nickname, and the two nodes do not
/// accept each other's frames. Neither holder sees the message of the other; an observer with its own
/// nickname still sees the mesh work. The key still decides who is the author: the echo rule is pinned by
/// `message_event_carries_full_pubkey` and the tests of the key-keyed roster.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_members_of_one_nickname_do_not_see_each_others_messages() {
    let mut alpha = InProcNode::create_with_nick("samenick", "dup").await;
    let mut beta = InProcNode::join(&alpha.mesh, "dup").await;
    let observer = InProcNode::join(&alpha.mesh, "samenick-obs").await;

    alpha.broadcast("from-alpha").await;
    beta.broadcast("from-beta").await;
    // A gossip holds no log of a broadcast, so a message sent before the links exist is lost:
    // the observer sends again until both holders have a message of it.
    let mut reached_both = false;
    for round in 0..6 {
        let body = format!("from-observer-{round}");
        observer.broadcast(&body).await;
        if alpha.wait_body(&body, Duration::from_secs(5)).await
            && beta.wait_body(&body, Duration::from_secs(5)).await
        {
            reached_both = true;
            break;
        }
    }
    assert!(
        reached_both,
        "the observer's message did not reach a holder: the mesh does not work at all"
    );
    assert!(
        !beta.wait_body("from-alpha", Duration::from_secs(5)).await,
        "beta saw the message of alpha: the gate admitted a second key under the nickname"
    );
    assert!(
        !alpha.wait_body("from-beta", Duration::from_secs(5)).await,
        "alpha saw the message of beta: the gate admitted a second key under the nickname"
    );

    alpha.leave().await;
    beta.leave().await;
    observer.leave().await;
}

/// The `--output json` `message` event exposes the author's full public key,
/// so an agent can key trust/disambiguation on the key rather than the
/// (non-unique) nickname.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_event_carries_full_pubkey() {
    let alpha = InProcNode::create("pubkeyjson").await;
    let mut beta = InProcNode::join(&alpha.mesh, "pk-beta").await;

    alpha.broadcast("hi").await;
    assert!(
        beta.wait_body("hi", MSG_TIMEOUT).await,
        "message never arrived"
    );

    let events: Vec<_> = beta.message_events().collect();
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
    let first = InProcNode::join(&observer.mesh, "ditto").await;

    first.broadcast("first-here").await;
    assert!(
        observer.wait_body("first-here", MSG_TIMEOUT).await,
        "observer never saw the first member"
    );
    first.leave().await;

    // A brand-new member reuses the departed nickname.
    let second = InProcNode::join(&observer.mesh, "ditto").await;
    second.broadcast("second-here").await;
    assert!(
        observer.wait_body("second-here", MSG_TIMEOUT).await,
        "a reused nickname could not communicate after the prior holder left"
    );

    observer.leave().await;
    second.leave().await;
}

/// A peer that dies without a graceful leave keeps its slice of the meta channel for about 1 hour,
/// and `meta get` must say so.
///
/// Only the author of a slice can remove it: a graceful leave tombstones the slice, and a key that
/// dies is dropped by the engine only after its quiet time. The entry stays readable with whatever
/// status its agent last set, while the roster evicts the peer within the alive timeout. An agent
/// picking a peer to delegate to reads that document, so the staleness has to be visible.
///
/// The document itself is left intact — trimming it would invite a get/edit/merge round-trip to
/// write the trimmed view back as truth — so the answer rides alongside, in `absent`.
#[test]
fn meta_get_names_entries_whose_peer_is_gone() {
    let (survivor, mesh) = Node::create_flags("meta-ghost", &SHORT_EVICT);
    let ghost = Node::join_args(&mesh, "meta-ghost-peer", &[], &SHORT_EVICT);
    common::await_peer_card_cli(&mesh, &survivor.nickname, &ghost.nickname);
    common::await_peer_card_cli(&mesh, &ghost.nickname, &survivor.nickname);
    // The daemon writes no entry: each agent reports its own, as the join skill does. The slot of a
    // member is the hex of its author key; its entry names the nickname.
    common::cli_meta_report(&mesh, &survivor.nickname);
    common::cli_meta_report(&mesh, &ghost.nickname);
    let ghost_slot = common::await_meta_slot_cli(&mesh, &survivor.nickname, &ghost.nickname);
    let survivor_slot = common::await_meta_slot_cli(&mesh, &survivor.nickname, &survivor.nickname);

    // Ungraceful death: no `Left`, so no tombstone is ever authored.
    ghost.kill();

    // The survivor evicts it from the roster within the alive timeout, but the
    // meta entry cannot follow.
    let deadline = Instant::now() + RECOVERY_TIMEOUT;
    loop {
        let response: serde_json::Value = serde_json::from_str(&ipc_raw(
            &mesh,
            &survivor.nickname,
            &format!(r#"{{"command":"meta_get","mesh":"{mesh}"}}"#),
        ))
        .expect("meta_get response is JSON");

        let absent_list = response["absent"]
            .as_array()
            .expect("meta_get carries an absent list");
        let absent: Vec<&str> = absent_list
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();

        if absent.contains(&ghost_slot.as_str()) {
            // The entry is still in the document — reported, not deleted.
            assert!(
                response["document"][&ghost_slot].is_object(),
                "the entry must be reported, not trimmed: {response}"
            );
            // And the survivor never reports itself.
            assert!(
                !absent.contains(&survivor_slot.as_str()),
                "a node must not flag its own entry: {response}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "meta_get never reported the dead peer as absent: {response}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A row of `peers` names the member by its author key, which is the slot of the member in the
/// meta document. Two members can share a nickname, so a reader that joins the roster to the meta
/// document joins by the key and never by the name.
#[test]
fn the_peers_row_names_the_pubkey_of_its_meta_slot() {
    let (host, mesh) = Node::create_named("peers-key");
    let member = Node::join(&mesh, "peers-key-m");
    common::await_peer_card_cli(&mesh, &host.nickname, &member.nickname);
    common::cli_meta_report(&mesh, &member.nickname);
    let slot = common::await_meta_slot_cli(&mesh, &host.nickname, &member.nickname);

    let deadline = Instant::now() + RECOVERY_TIMEOUT;
    loop {
        let roster: serde_json::Value = serde_json::from_str(&cli_peers(&mesh, &host.nickname))
            .expect("peers prints one JSON object");
        let rows: &[serde_json::Value] = roster["peers"].as_array().map_or(&[], Vec::as_slice);
        let row = rows
            .iter()
            .find(|row| row["nickname"] == member.nickname.as_str());
        if let Some(row) = row {
            assert_eq!(
                row["pubkey"], slot,
                "the row does not name the key of the meta slot: {row}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the member never reached the roster: {roster}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ── starvation watchdog (the roster-collapse fix) ─────────────────────────────

// `SHORT_EVICT` plus a 6s starvation threshold. The threshold is its own
// flag (not derived from the alive timeout) precisely so the short-evict
// profile used across this suite never arms the watchdog by accident;
// these tests opt in explicitly.
const STARVE_EVICT: [(&str, &str); 4] = [
    ("--alive-timeout-secs", "3"),
    ("--sweep-interval-secs", "1"),
    ("--heal-interval-secs", "3"),
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
    // Threshold (6s) + a couple of heal ticks at the injected cadence +
    // margin. Adaptive — ceiling only.
    let detect = Duration::from_secs(6 + 2 * TEST_HEAL_SECS + 8);

    let (creator, mesh) = Node::create_flags("itest", &STARVE_EVICT);
    let survivor = Node::join_flags(&mesh, "sv-alpha", &STARVE_EVICT);
    assert!(
        creator.wait_ready(&mesh),
        "the creator daemon must bind its control socket before the test sends it any command"
    );
    assert!(
        survivor.wait_ready(&mesh),
        "the survivor daemon must bind its control socket before the test sends it any command"
    );
    let _ = cli_message(&mesh, &creator.nickname, "sv-base");
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
    let _ = common::cli_message_checked(&mesh, &survivor.nickname, "sv-after");
}

/// False-positive guard: a lone creator is alone by construction — it
/// never announced into a mesh of real peers and knows nobody to
/// re-dial — so the watchdog must stay silent no matter how long it
/// idles past the threshold.
#[test]
fn test_lone_creator_never_trips_starvation() {
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    let (creator, mesh) = Node::create_flags("itest", &STARVE_EVICT);
    assert!(
        creator.wait_ready(&mesh),
        "the creator daemon must bind its control socket before the test sends it any command"
    );
    // Threshold (6s) + several heal ticks of opportunity to misfire —
    // an inherent blind wait (negative assertion), derived from the
    // injected cadence.
    std::thread::sleep(Duration::from_secs(6 + 3 * TEST_HEAL_SECS));
    assert!(
        !creator.log_contents().contains("mesh starvation"),
        "lone creator false-tripped the starvation watchdog\n{}",
        creator.log_tail(25),
    );
}

/// The 2026-05-31 roster-collapse, mechanized: a 10-node mesh at
/// `--max-connections 8` (the lowest cap the engine accepts: a node holds 8 of the 9 others, so
/// the mesh is partial) put through SIGSTOP/SIGCONT flap rounds. Pre-fix, a node could end up
/// with an empty roster and phantom links forever — silent message
/// loss. Post-fix (link truth + starvation watchdog), every node must
/// deliver again once the storm passes.
#[test]
fn test_flap_storm_all_rosters_recover() {
    const CAP8_STARVE: [(&str, &str); 5] = [
        ("--alive-timeout-secs", "3"),
        ("--sweep-interval-secs", "1"),
        ("--heal-interval-secs", "3"),
        ("--starvation-threshold-secs", "6"),
        ("--max-connections", "8"),
    ];
    // Serialize against the other timing-sensitive tests (see `serial_guard`).
    let _serial = serial_guard();
    // Stop window: past the 3+1s evict so victims get swept; resume gap
    // a couple of heal ticks so partial re-meshing happens before the
    // next round hits.
    let stop_window = Duration::from_secs(8);
    let resume_gap = Duration::from_secs(2 * TEST_HEAL_SECS);
    // Recovery bound: starvation threshold (6s) + a couple of heal ticks
    // for re-bridge/re-announce to propagate, plus margin.
    let recover = RECOVERY_TIMEOUT;

    let (creator, mesh) = Node::create_flags("itest", &CAP8_STARVE);
    let joiners: Vec<Node> = (0..9)
        .map(|index| Node::join_flags(&mesh, &format!("fs-{index}"), &CAP8_STARVE))
        .collect();
    assert!(
        creator.wait_ready(&mesh),
        "the creator daemon must bind its control socket before the test sends it any command"
    );
    for joiner in &joiners {
        assert!(joiner.wait_ready(&mesh), "{} never ready", joiner.nickname);
    }
    // Baseline delivery gets the same generous bound as the post-storm
    // probe: at cap 8 with ten nodes even a healthy broadcast is partial-mesh-routed
    // (multi-hop, convergence-dependent), so the standard 30s message
    // timeout is occasionally short here. Adaptive — healthy runs pass
    // in seconds.
    let _ = cli_message(&mesh, &creator.nickname, "fs-base");
    for joiner in &joiners {
        assert_received(joiner, &creator.nickname, "fs-base", recover);
    }

    // Five flap rounds, four victims each, rotating across all ten nodes, so that every node is
    // stopped twice. The first storm had 5 nodes and stopped 2 of them in a round, the same 40
    // percent; with 10 nodes that is 4. The shape wedged the soak and the manual repro
    // (roster_len=0 with phantom links, delivery frozen).
    let all: Vec<&Node> = std::iter::once(&creator).chain(joiners.iter()).collect();
    for round in 0..5usize {
        let victims: Vec<&Node> = [0, 1, 5, 6]
            .iter()
            .map(|offset| all[(2 * round + offset) % all.len()])
            .collect();
        for victim in &victims {
            victim.stop();
        }
        std::thread::sleep(stop_window);
        for victim in &victims {
            victim.cont();
        }
        std::thread::sleep(resume_gap);
    }

    // Settle a couple of heal ticks, then the invariant: a fresh
    // broadcast reaches EVERY node.
    std::thread::sleep(Duration::from_secs(2 * TEST_HEAL_SECS));
    let _ = cli_message(&mesh, &creator.nickname, "fs-probe");
    for joiner in &joiners {
        assert_received(joiner, &creator.nickname, "fs-probe", recover);
    }
}

// ── leave / session (session-scope daemon discovery) ─────────────────────────

/// Spawn a create daemon with `--output json` and block until its `ready`
/// event appears on stdout, returning the child plus its minted identity.
/// The default state-file location (under the per-user runtime base) is what `leave` /
/// `session` discover, so no `--state-file` override here.
fn spawn_discoverable_daemon(name: &str) -> (KillOnDrop, PathBuf, String, String) {
    let log = tmp_log(&format!("leave-{name}"));
    let file = File::create(&log).expect("the test temp dir is writable");
    let stdout = Stdio::from(
        file.try_clone()
            .expect("the log file handle can be cloned for stdout"),
    );
    let mut command = common::test_cmd();
    command.args(["create", "--name", name, "--lookup", "loopback"]);
    let mut daemon = KillOnDrop::new(
        command
            .stdout(stdout)
            .stderr(Stdio::from(file))
            .spawn()
            .expect("failed to spawn create"),
    );
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        if let Some((mesh, nickname)) = ready_identity(&log) {
            return (daemon, log, mesh, nickname);
        }
        if Instant::now() >= deadline {
            let _ = daemon.kill();
            let _ = daemon.wait();
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
        ready["gossip"].as_str()?.to_owned(),
        ready["nickname"].as_str()?.to_owned(),
    ))
}

fn default_state_file(mesh: &str, nickname: &str) -> PathBuf {
    // Mirror `util::mesh_prefix`: strip the `://` scheme separator before
    // taking 16 chars, so the path matches where the daemon writes its state.
    let without_scheme: String = mesh.split("://").collect();
    let prefix: String = without_scheme.chars().take(16).collect();
    common::runtime_base()
        .join(prefix)
        .join(format!("{nickname}.state.json"))
}

/// The state file must carry the daemon's own pid — what lets `leave` /
/// `session` map the file back to a live process and to the agent session
/// that spawned it.
#[test]
fn state_file_carries_daemon_pid() {
    let (mut child, log, mesh, nickname) = spawn_discoverable_daemon("leave-pid");
    let state_file = default_state_file(&mesh, &nickname);
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_file).expect("the test file is readable"))
            .expect("the state file holds JSON written by the daemon");
    assert_eq!(
        parsed["pid"],
        child.id(),
        "the state file records the daemon pid"
    );

    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();
    let _ = fs::remove_file(&log);
}

/// A CLI daemon started with no `--state-file` keeps its member key and its run counter beside the default
/// state file, so a restart after a crash is the same member and counts its run on: an ephemeral key would
/// show the peers a new member under the nickname that the old session still holds.
#[test]
fn a_daemon_with_no_state_file_keeps_its_member_key_in_the_default_state_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let (mut child, log, mesh, nickname) = spawn_discoverable_daemon("keys-default");
    let key_file = default_state_file(&mesh, &nickname).with_extension("key");

    let held = fs::read(&key_file);
    let mode = fs::metadata(&key_file).map(|meta| meta.permissions().mode() & 0o777);

    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();
    let _ = fs::remove_file(&log);
    let bytes = held.expect("the daemon kept no member key beside its default state file");
    assert!(
        bytes.len() >= 40,
        "the key file holds the seed and the owner start"
    );
    assert_eq!(
        mode.ok(),
        Some(0o600),
        "the key file is private to the user"
    );
}

/// A member key file that others can read is not loaded: the daemon stops, and the user is told which file it is
/// and how to go on (remove it), on the text of the process and not only in a log.
#[test]
fn a_member_key_file_that_others_can_read_stops_the_daemon_and_names_the_file() {
    use std::os::unix::fs::PermissionsExt as _;
    let (creator, mesh) = Node::create_named("keytrust");
    let dir = std::env::temp_dir().join(format!("agent-gossip-keytrust-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("a temp folder");
    let state = dir.join("session.json");
    let key = state.with_extension("key");
    fs::write(&key, [3u8; 40]).expect("the folder of the test is writable");
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644))
        .expect("the owner can widen the mode, so that others can read the file");

    let out = common::test_cmd()
        .args([
            "join",
            &mesh,
            "--nickname",
            "keytrust-joiner",
            "--state-file",
        ])
        .arg(&state)
        .output()
        .expect("agent-gossip join runs");

    creator.kill();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_dir_all(&dir);
    assert!(
        !out.status.success(),
        "the daemon started with a key file that others can read: {text}"
    );
    assert!(
        text.contains(&key.display().to_string()) && text.contains("remove it"),
        "the user is not told which key file is refused and how to go on: {text}"
    );
}

/// `agent-gossip leave <id>` (explicit target) stops exactly that mesh's local
/// daemon — the state file disappears (proof of the graceful shutdown path)
/// — and leaves an unrelated daemon untouched.
#[test]
fn leave_explicit_target_stops_only_that_mesh() {
    let (mut victim, victim_log, victim_mesh, victim_nick) =
        spawn_discoverable_daemon("leave-victim");
    let (mut bystander, bystander_log, bystander_mesh, bystander_nick) =
        spawn_discoverable_daemon("leave-bystander");

    let out: std::process::Output = common::test_cmd()
        .args(["leave", &victim_mesh])
        .output()
        .expect("failed to run agent-gossip leave");
    assert!(
        out.status.success(),
        "a daemon that is running must accept the leave and exit zero, output was {out:?}"
    );
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("the JSON text comes from the test itself");
    let left = report["left"]
        .as_array()
        .expect("the JSON field is an array in this test");
    assert_eq!(left.len(), 1, "expected exactly the victim: {report}");
    assert_eq!(
        left[0]["gossip"],
        victim_mesh.as_str(),
        "the leave report names the victim gossip"
    );
    assert_eq!(left[0]["confirmed"], true, "the victim is confirmed left");
    assert!(
        !default_state_file(&victim_mesh, &victim_nick).exists(),
        "victim state file survived leave"
    );
    assert!(
        default_state_file(&bystander_mesh, &bystander_nick).exists(),
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

/// `peers`, `topology` and `poll` take the gossip as `--gossip <id>`, so `leave` takes it
/// in that spelling too, with `--nickname` beside it.
#[test]
fn leave_takes_the_gossip_as_a_flag() {
    let (mut daemon, log, mesh, nickname) = spawn_discoverable_daemon("leave-flag");

    let mut command = common::test_cmd();
    command.args(["leave", "--gossip", &mesh, "--nickname", &nickname]);
    let out: std::process::Output = command.output().expect("failed to run leave");
    assert!(
        out.status.success(),
        "the flag form is refused as an unexpected argument: {out:?}"
    );
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("leave prints JSON on success");
    let left = report["left"]
        .as_array()
        .expect("the JSON field is an array in this test");
    assert_eq!(
        left.len(),
        1,
        "the flag names the one daemon to stop: {report}"
    );
    assert_eq!(
        left[0]["confirmed"], true,
        "the daemon is still serving: {report}"
    );
    assert!(
        !default_state_file(&mesh, &nickname).exists(),
        "the daemon removes its own state file on the way out"
    );
    let _ = daemon.wait();
    let _ = fs::remove_file(&log);
}

/// Three skills tell an agent "if the ready output carries `drift`, print it
/// verbatim" — so `ready` has to be able to carry it.
///
/// It could not. The daemon computes the stale-skill warning onto its own
/// `--output json` stdout, which is the exact stream the session-start script
/// sends to /dev/null, and the `ready` subcommand only ever emitted
/// gossip/name/nickname/topic. Every agent following the docs was blind to a
/// skill install that had fallen behind its binary.
#[test]
fn ready_carries_the_stale_skill_warning() {
    let (mut child, log, mesh, nickname) = spawn_discoverable_daemon("ready-drift");
    let state_file = default_state_file(&mesh, &nickname);
    let home = std::env::temp_dir().join(format!("agent-gossip-drift-home-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    // `plug` installs only into agents it detects, and detection is the
    // agent's own dotdir existing.
    fs::create_dir_all(home.join(".codex")).expect("the test temp directory is writable");

    let ready: Box<dyn Fn(&std::path::Path) -> serde_json::Value + '_> =
        Box::new(|fake_home| -> serde_json::Value {
            let state = state_file.to_str().expect("the test path is valid UTF-8");
            let out: std::process::Output = common::test_cmd()
                .args(["ready", "--state-file", state])
                .env("HOME", fake_home)
                .output()
                .expect("failed to run agent-gossip ready");
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
                .expect("the JSON text comes from the test itself")
        });

    // A fresh install of this very binary is not drift.
    let plugged = common::test_cmd()
        .args(["plug", "--agent", "codex"])
        .env("HOME", &home)
        .output()
        .expect("failed to run agent-gossip plug");
    assert!(plugged.status.success(), "plug failed: {plugged:?}");
    let clean = ready(&home);
    assert_eq!(
        clean["nickname"],
        nickname.as_str(),
        "the clean report names the nickname"
    );
    assert!(
        clean["drift"].is_null(),
        "a matching install must not warn: {clean}"
    );

    // Diverge one installed skill and the warning must reach the agent.
    fs::write(home.join(".codex/skills/gossip-join/SKILL.md"), "stale")
        .expect("the test temp directory is writable");
    let stale = ready(&home);

    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();
    let _ = fs::remove_file(&log);
    let _ = fs::remove_dir_all(&home);

    assert!(
        stale["drift"]
            .as_str()
            .is_some_and(|message| message.contains("agent-gossip plug")),
        "a diverged install must surface drift on ready: {stale}"
    );
}

/// `confirmed: true` must mean the daemon has stopped serving, not merely that
/// its state file is gone.
///
/// The daemon removes that file early in its shutdown and then spends the rest
/// of the teardown still in the mesh and still answering its socket. `leave`
/// polled for the file, so it returned roughly half a second before the daemon
/// actually went away — and a caller that trusted `confirmed` and started a
/// replacement got two daemons on one identity, the new one announcing `joined`
/// while the old one had yet to broadcast `left`.
#[test]
fn leave_confirms_only_when_the_daemon_has_stopped_serving() {
    let (mut child, log, mesh, nickname) = spawn_discoverable_daemon("leave-serving");

    let out: std::process::Output = common::test_cmd()
        .args(["leave", &mesh])
        .output()
        .expect("failed to run agent-gossip leave");
    assert!(
        out.status.success(),
        "a daemon that is running must accept the leave and exit zero, output was {out:?}"
    );
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("the JSON text comes from the test itself");
    assert_eq!(
        report["left"][0]["confirmed"], true,
        "leave confirms only after the daemon stopped serving; report: {report}"
    );

    // The daemon's own socket is the black-box probe: `peers` reaches it from
    // the nickname alone — no state file involved — so it answers for exactly
    // as long as the daemon is up.
    let probe = common::test_cmd()
        .args(["peers", "--gossip", &mesh, "--nickname", &nickname])
        .output()
        .expect("failed to run agent-gossip peers");
    let _ = child.wait();
    let _ = fs::remove_file(&log);
    assert!(
        !probe.status.success(),
        "leave reported confirmed while the daemon was still answering: {}",
        String::from_utf8_lossy(&probe.stdout).trim()
    );
    // The probe only means something if it fails for the right reason.
    let why = String::from_utf8_lossy(&probe.stderr);
    assert!(
        why.contains("No active gossip server running"),
        "peers failed for an unrelated reason: {why}"
    );
}

/// A topic daemon persists the raw string it was derived from: the state file
/// and the `agent-gossip leave` report both carry it as `topic`, whole —
/// query string and all. The derived name is lossy (`www.youtube.com/watch`
/// for a watch URL), and the skills echo the string on join/leave.
#[test]
fn leave_report_carries_the_full_topic_string() {
    let topic = format!(
        "https://www.youtube.com/watch?v=leave-{}",
        std::process::id()
    );
    let log = tmp_log("leave-topic");
    let file = File::create(&log).expect("the test log directory is writable");
    let stdout = Stdio::from(file.try_clone().expect("the test log file handle is open"));
    let mut command = common::test_cmd();
    command.args(["topic", &topic]);
    let mut child: Child = command
        .stdout(stdout)
        .stderr(Stdio::from(file))
        .spawn()
        .expect("the test binary is built before the suite runs, so its path always spawns");
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let (mesh, nickname) = loop {
        if let Some(identity) = ready_identity(&log) {
            break identity;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "topic daemon never emitted ready\nlog:\n{}",
                fs::read_to_string(&log).unwrap_or_default()
            );
        }
        std::thread::sleep(POLL);
    };

    let state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(default_state_file(&mesh, &nickname))
            .expect("the test file is readable"),
    )
    .expect("the state file holds JSON written by the daemon");
    assert_eq!(
        state["topic"],
        topic.as_str(),
        "the state file records the topic"
    );

    let out: std::process::Output = common::test_cmd()
        .args(["leave", &mesh])
        .output()
        .expect("failed to run agent-gossip leave");
    assert!(
        out.status.success(),
        "a daemon that is running must accept the leave and exit zero, output was {out:?}"
    );
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("the JSON text comes from the test itself");
    let left = report["left"]
        .as_array()
        .expect("the JSON field is an array in this test");
    assert_eq!(left.len(), 1, "expected exactly the topic daemon: {report}");
    assert_eq!(
        left[0]["topic"],
        topic.as_str(),
        "the leave report names the topic"
    );
    assert_eq!(
        left[0]["confirmed"], true,
        "the topic gossip is confirmed left"
    );

    let _ = child.wait();
    let _ = fs::remove_file(&log);
}

/// A daemon under a stand-in agent process: a shell that runs `create`, so a
/// test can pass the shell's pid as `--session-pid`. Returns the shell, its
/// pid, the gossip id, the nickname, and the daemon's log.
fn spawn_decoy(name: &str) -> (Child, String, String, String, PathBuf) {
    let log = tmp_log(name);
    // The trailing `:` defeats the shell's exec-of-last-command optimization,
    // keeping the shell alive as the daemon's parent — the ancestry link the
    // session scope matches on.
    let script = format!(
        "{} --log-dir {} create --name {name} > {} 2>&1; :",
        bin().display(),
        common::test_log_dir(),
        log.display(),
    );
    let decoy: Child = Command::new("sh")
        .arg("-c")
        .arg(script)
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
    let (mesh, nickname) =
        ready_identity(&log).expect("the daemon wrote its ready line before this read");
    (decoy, decoy_pid, mesh, nickname, log)
}

/// Run `command` to its end, or kill it and fail the test after `limit`. A hang in the code under
/// test then fails the test, and a `StoppedDaemon` guard still resumes its daemon.
fn output_within(command: &mut Command, limit: Duration) -> std::process::Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .expect("the test binary is built before the suite runs, so its path always spawns");
    let deadline = Instant::now() + limit;
    loop {
        let exited = child
            .try_wait()
            .expect("a child of this test can be polled");
        if exited.is_some() {
            return child
                .wait_with_output()
                .expect("an exited child hands over its output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{command:?} ran past {limit:?}, so the daemon never answered it");
        }
        std::thread::sleep(POLL);
    }
}

/// The longest a `session` or `leave` query may take in these tests. Leave waits up to its own
/// budget for a daemon to stop, so the bound sits well above it.
const QUERY_LIMIT: Duration = Duration::from_secs(90);

fn session_report(session_pid: &str) -> serde_json::Value {
    let mut command = common::test_cmd();
    command.args(["session", "--session-pid", session_pid]);
    let out: std::process::Output = output_within(&mut command, QUERY_LIMIT);
    assert!(
        out.status.success(),
        "the session query needs no daemon and exits zero, output was {out:?}"
    );
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .expect("the session command prints JSON on success")
}

/// Session scope end to end: a daemon spawned under a decoy "agent" shell is
/// owned by that shell's pid. `agent-gossip session --session-pid <shell>` reports it
/// without touching it; `agent-gossip leave --session-pid <shell>` stops it. Daemons
/// belonging to other tests (children of this test binary, not of the decoy
/// shell) must never match.
#[test]
fn leave_session_scope_via_decoy_parent() {
    let _serial = serial_guard();

    let (mut decoy, decoy_pid, mesh, nickname, log) = spawn_decoy("leave-decoy");

    // Read-only probe: reports the decoy's daemon, does not stop it.
    let report = session_report(&decoy_pid);
    let sessions = report["sessions"]
        .as_array()
        .expect("the JSON field is an array in this test");
    assert_eq!(sessions.len(), 1, "expected exactly the decoy: {report}");
    assert_eq!(
        sessions[0]["gossip"],
        mesh.as_str(),
        "the session report names the gossip"
    );
    assert_eq!(
        sessions[0]["nickname"],
        nickname.as_str(),
        "the session report names the nickname"
    );
    // A reattached skill holds no key from the ready gate, and the meta slot of its entry is the
    // hex of that key, so the session report names it.
    let pubkey: &str = sessions[0]["pubkey"]
        .as_str()
        .unwrap_or_else(|| panic!("the session report names no pubkey: {report}"));
    assert!(
        pubkey.len() == 64 && pubkey.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the pubkey is 64 hex digits: {pubkey}"
    );
    assert!(
        default_state_file(&mesh, &nickname).exists(),
        "session (read-only) stopped the daemon"
    );

    let leave_out = common::test_cmd()
        .args(["leave", "--session-pid", &decoy_pid])
        .output()
        .expect("failed to run agent-gossip leave");
    assert!(
        leave_out.status.success(),
        "the leave resolves the session from the decoy parent and exits zero, output was {leave_out:?}"
    );
    let leave_report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&leave_out.stdout).trim())
            .expect("the JSON text comes from the test itself");
    let left = leave_report["left"]
        .as_array()
        .expect("the JSON field is an array in this test");
    assert_eq!(left.len(), 1, "expected exactly the decoy: {leave_report}");
    assert_eq!(
        left[0]["gossip"],
        mesh.as_str(),
        "the leave report names the gossip"
    );
    assert_eq!(left[0]["confirmed"], true, "the gossip is confirmed left");
    assert!(
        !default_state_file(&mesh, &nickname).exists(),
        "the default state file is removed"
    );

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

    let idle_pid = idle.id().to_string();
    let out: std::process::Output = common::test_cmd()
        .args(["leave", "--session-pid", &idle_pid])
        .output()
        .expect("failed to run agent-gossip leave");
    assert!(out.status.success(), "leave should exit 0 on a no-op");
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("the JSON text comes from the test itself");
    assert_eq!(report["ok"], true, "the no-op leave reports ok");
    assert_eq!(
        report["left"]
            .as_array()
            .expect("the JSON field is an array in this test")
            .len(),
        0,
        "a no-op leave leaves nothing"
    );

    let _ = idle.kill();
    let _ = idle.wait();
}

/// The Stop hook's probe must block nothing for an agent that is not in a
/// gossip.
#[test]
fn bell_check_passes_without_a_session() {
    let mut idle = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("failed to spawn sleep");

    let out = bell_check(&idle.id().to_string());
    assert!(
        out.status.success(),
        "a session with no bell must exit zero so the hook never blocks the agent, output was {out:?}"
    );
    assert!(out.stdout.is_empty(), "no session must not block: {out:?}");

    let _ = idle.kill();
    let _ = idle.wait();
}

fn bell_check(session_pid: &str) -> std::process::Output {
    common::test_cmd()
        .args([
            "bell-check",
            "--session-pid",
            session_pid,
            "--grace-ms",
            "0",
        ])
        .output()
        .expect("failed to run agent-gossip bell-check")
}

/// The Stop-hook decision `bell-check` printed, or `None` when it let the
/// turn end.
fn bell_check_block_reason(session_pid: &str) -> Option<String> {
    let out = bell_check(session_pid);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stdout = stdout.trim();
    if stdout.is_empty() {
        assert!(out.status.success(), "a pass exits 0: {out:?}");
        return None;
    }
    // A shell `bell-check && echo armed` must not lie; the Stop hook's
    // `|| true` still hands Claude Code the JSON.
    assert_eq!(out.status.code(), Some(3), "a block exits 3: {out:?}");
    let decision: serde_json::Value =
        serde_json::from_str(stdout).expect("bell-check prints JSON when it blocks");
    assert_eq!(
        decision["decision"], "block",
        "the bell check blocks the stop"
    );
    Some(
        decision["reason"]
            .as_str()
            .expect("a block decision carries a reason string")
            .to_owned(),
    )
}

fn wait_for_bell(session_pid: &str, armed: bool) {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while session_report(session_pid)["sessions"][0]["bell"] != armed {
        assert!(
            Instant::now() < deadline,
            "session never reported bell:{armed}"
        );
        std::thread::sleep(POLL);
    }
}

/// `session` and `bell-check` read the bell from the lock a live `poll --long`
/// holds: both bell forms take it, the OS drops it when the bell dies, and a
/// settle delay does not open a window with no bell.
#[test]
fn bell_check_and_session_track_the_bell_lock() {
    let _serial = serial_guard();
    let (mut decoy, decoy_pid, mesh, nickname, log) = spawn_decoy("bell-lock");

    assert_eq!(
        session_report(&decoy_pid)["sessions"][0]["bell"],
        false,
        "the decoy session is not bell-armed"
    );
    let reason = bell_check_block_reason(&decoy_pid).expect("no bell blocks the turn");
    assert!(
        reason.contains("gossip bell not armed") && reason.contains(nickname.as_str()),
        "the reason names the problem and the session: {reason}"
    );

    let mut command = common::test_cmd();
    command
        .args(["poll", "--state-file"])
        .arg(default_state_file(&mesh, &nickname))
        .arg("--long");
    let mut session_start_bell: Child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn the state-file bell");
    wait_for_bell(&decoy_pid, true);
    assert_eq!(bell_check_block_reason(&decoy_pid), None, "armed passes");

    let _ = session_start_bell.kill();
    let _ = session_start_bell.wait();
    wait_for_bell(&decoy_pid, false);
    assert!(
        bell_check_block_reason(&decoy_pid).is_some(),
        "a killed bell is gone"
    );

    command = common::test_cmd();
    command
        .args(["poll", "--gossip", &mesh, "--nickname", &nickname, "--long"])
        .args(["--settle-secs", "30"]);
    let mut settling_bell: Child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn the re-armed bell");
    wait_for_bell(&decoy_pid, true);
    assert_eq!(
        bell_check_block_reason(&decoy_pid),
        None,
        "a settling bell already counts"
    );

    let _ = settling_bell.kill();
    let _ = settling_bell.wait();
    let _ = common::test_cmd()
        .args(["leave", "--session-pid", &decoy_pid])
        .output();
    let _ = decoy.wait();
    let _ = fs::remove_file(&log);
}

/// Gossip carries no directed traffic, so a directed A2A task that
/// round-trips (request out, worker-minted id back) proves the unicast
/// transport delivers point-to-point — not via the gossip flood. Broadcasts
/// still ride gossip, so the warmup meshes the daemons and exchanges the
/// `PeerInfo` the sender's unicast dial resolves on.
#[test]
fn directed_task_delivers_point_to_point() {
    let (creator, mesh) = Node::create_named("p2p-task");
    let joiner = Node::join(&mesh, "p2p-task-b");
    assert!(
        creator.wait_ready(&mesh),
        "the creator daemon must bind its control socket before the test sends it any command"
    );
    assert!(
        joiner.wait_ready(&mesh),
        "the joiner daemon must bind its control socket before the test sends it any command"
    );

    // Warmup broadcast (always gossip) meshes them + exchanges addresses.
    let _ = cli_message(&mesh, &creator.nickname, "warmup");
    assert!(
        wait_until(
            || joiner.count_from(&creator.nickname, "warmup"),
            1,
            MSG_TIMEOUT
        ) >= 1,
        "warmup broadcast never reached the joiner (mesh didn't form)"
    );

    // Directed task: gossip won't carry the A2aReq/A2aResp, so a round-trip (a
    // worker-minted task id) can only have gone point-to-point. The send fails
    // until the sender has learned the worker's endpoint (its signed
    // `PeerInfo`) and a dial lands, so retry until it does — a success proves
    // unicast delivered both legs.
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let out = cli_task_create_raw(&mesh, &creator.nickname, &joiner.nickname, "p2p hello");
        if out.status.success()
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(
                String::from_utf8_lossy(&out.stdout).trim(),
            )
            && parsed["result"]["task"]["id"].is_string()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "directed task never round-tripped — unicast failed to deliver"
        );
        std::thread::sleep(POLL);
    }
}

struct DetachedDaemon {
    owner: KillOnDrop,
    state_file: PathBuf,
    launcher_pid: u32,
    launcher_stderr: String,
    launch_time: Duration,
}

/// Launch `create --owner-pid` for a `sleep` owner and wait for the launcher
/// to exit. The state file sits in the runtime base, where `session` discovery
/// walks for it.
fn launch_detached_daemon(name: &str) -> DetachedDaemon {
    launch_detached_daemon_in(name, &common::runtime_base().join("sessions"))
}

/// Like [`launch_detached_daemon`], with the state file in `state_dir`.
fn launch_detached_daemon_in(name: &str, state_dir: &std::path::Path) -> DetachedDaemon {
    let owner = KillOnDrop::new(
        Command::new("sleep")
            .arg("600")
            .spawn()
            .expect("a long sleep stands in for the owner process and exists on every unix host"),
    );
    fs::create_dir_all(state_dir).expect("the test temp dir is writable");
    let state_file = state_dir.join(format!("{}.json", owner.id()));
    let stderr_file = tmp_log(&format!("{name}-stderr"));

    let started = Instant::now();
    let owner_pid = owner.id().to_string();
    let stderr = File::create(&stderr_file).expect("the test temp dir is writable");
    let mut command = common::test_cmd();
    command
        .args([
            "create",
            "--name",
            name,
            "--lookup",
            "loopback",
            "--owner-pid",
        ])
        .arg(owner_pid)
        .arg("--state-file")
        .arg(&state_file);
    command.stdin(Stdio::null()).stdout(Stdio::null());
    let mut launcher: Child = command
        .stderr(stderr)
        .spawn()
        .expect("the test binary is built before the suite runs, so its path always spawns");
    let launcher_pid = launcher.id();
    let status = launcher
        .wait()
        .expect("the launcher child can be waited on");
    let launch_time = started.elapsed();
    let launcher_stderr = fs::read_to_string(&stderr_file).unwrap_or_default();
    assert!(
        status.success(),
        "launcher failed: {status}\nstderr:\n{launcher_stderr}"
    );
    let _ = fs::remove_file(&stderr_file);
    DetachedDaemon {
        owner,
        state_file,
        launcher_pid,
        launcher_stderr,
        launch_time,
    }
}

fn wait_until_gone(path: &std::path::Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while path.exists() && Instant::now() < deadline {
        std::thread::sleep(POLL);
    }
    !path.exists()
}

/// `--owner-pid` detaches the daemon from its launcher and ties its life to
/// the owner: the launcher returns at once, the daemon survives it, `session`
/// and `bell-check` find it through the owner, and it stops with the owner.
#[test]
fn detached_daemon_follows_its_owner() {
    let _serial = serial_guard();
    let mut daemon = launch_detached_daemon("detach-owner");

    assert!(
        daemon.launch_time < Duration::from_secs(2),
        "the launcher took {:?}",
        daemon.launch_time
    );
    assert!(
        daemon.launcher_stderr.is_empty(),
        "the launcher wrote to stderr: {}",
        daemon.launcher_stderr
    );

    let mut command = common::test_cmd();
    command
        .arg("ready")
        .arg("--state-file")
        .arg(&daemon.state_file);
    let ready: std::process::Output = command.output().expect("failed to run ready");
    assert!(
        ready.status.success(),
        "the ready query must succeed once the detached daemon is up, output was {ready:?}"
    );

    let state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&daemon.state_file).expect("the test file is readable"),
    )
    .expect("the state file holds JSON written by the daemon");
    let daemon_pid = state["pid"].as_u64().expect("state file carries the pid");
    let ps_field = |field: &str| {
        let out = Command::new("ps")
            .args(["-o", field, "-p", &daemon_pid.to_string()])
            .output()
            .expect("`ps` is on PATH on every unix host, so it spawns");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    assert_eq!(
        ps_field("pgid="),
        daemon_pid.to_string(),
        "the daemon is not the leader of its own process group"
    );
    let parent_pid = ps_field("ppid=");
    assert_ne!(
        parent_pid,
        daemon.launcher_pid.to_string(),
        "the daemon is still a child of the launcher"
    );

    let owner_pid = daemon.owner.id().to_string();
    let sessions = session_report(&owner_pid);
    assert_eq!(
        sessions["sessions"]
            .as_array()
            .expect("the JSON field is an array in this test")
            .len(),
        1,
        "session does not see the daemon through its owner: {sessions}"
    );
    assert!(
        bell_check_block_reason(&owner_pid).is_some(),
        "bell-check does not see the daemon through its owner"
    );

    let _ = daemon.owner.kill();
    let _ = daemon.owner.wait();
    assert!(
        wait_until_gone(&daemon.state_file, Duration::from_secs(15)),
        "the daemon outlived its owner"
    );
}

/// `leave --session-pid <owner>` reaches a detached daemon, which is no
/// longer a descendant of any agent process.
#[test]
fn detached_daemon_leaves_by_owner() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("detach-leave");
    let mut command = common::test_cmd();
    command
        .arg("ready")
        .arg("--state-file")
        .arg(&daemon.state_file);
    let ready: std::process::Output = command.output().expect("failed to run ready");
    assert!(
        ready.status.success(),
        "the ready query must succeed once the detached daemon is up, output was {ready:?}"
    );

    let owner_pid = daemon.owner.id().to_string();
    let out: std::process::Output = common::test_cmd()
        .args(["leave", "--session-pid", &owner_pid])
        .output()
        .expect("failed to run leave");
    assert!(out.status.success(), "leave failed: {out:?}");
    let report: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("the JSON text comes from the test itself");
    assert_eq!(
        report["left"]
            .as_array()
            .expect("the JSON field is an array in this test")
            .len(),
        1,
        "leave missed the detached daemon: {report}"
    );
    assert!(
        wait_until_gone(&daemon.state_file, Duration::from_secs(5)),
        "the state file survived leave"
    );
}

/// Wait until the daemon that writes `state_file` reports ready.
fn wait_ready(state_file: &std::path::Path) {
    let mut command = common::test_cmd();
    command.arg("ready").arg("--state-file").arg(state_file);
    let ready: std::process::Output = command
        .output()
        .expect("the test binary is built before the suite runs, so its path always spawns");
    assert!(
        ready.status.success(),
        "a session query before ready races the daemon's start: {ready:?}"
    );
}

/// Run `leave --session-pid` and parse its report.
fn leave_by_owner(owner_pid: &str) -> serde_json::Value {
    let mut command = common::test_cmd();
    command.args(["leave", "--session-pid", owner_pid]);
    let out: std::process::Output = output_within(&mut command, QUERY_LIMIT);
    assert!(
        out.status.success(),
        "a daemon that cannot be stopped exits non-zero: {out:?}"
    );
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .expect("leave prints JSON on success")
}

/// `leave` and `session` find a daemon whose `--state-file` is outside the
/// runtime base: its IPC socket is always inside it, so the socket leads to
/// the state file.
#[test]
fn a_daemon_with_a_state_file_outside_the_runtime_base_is_found_and_left() {
    let _serial = serial_guard();
    let outside = std::env::temp_dir().join(format!("outside-base-{}", std::process::id()));
    assert!(
        !outside.starts_with(common::runtime_base()),
        "inside the runtime base the old walk finds the file, and the test proves nothing"
    );
    let daemon = launch_detached_daemon_in("outside-base", &outside);
    wait_ready(&daemon.state_file);

    let owner_pid = daemon.owner.id().to_string();
    let sessions = session_report(&owner_pid);
    assert_eq!(
        sessions["sessions"]
            .as_array()
            .expect("the JSON field is an array in this test")
            .len(),
        1,
        "the daemon is listed once, not missed and not doubled: {sessions}"
    );

    let report = leave_by_owner(&owner_pid);
    let left = report["left"]
        .as_array()
        .expect("the JSON field is an array in this test");
    assert_eq!(
        left.len(),
        1,
        "leave reports the daemon it stopped: {report}"
    );
    assert_eq!(
        left[0]["confirmed"], true,
        "a daemon still serving after leave is not left: {report}"
    );
    assert!(
        wait_until_gone(&daemon.state_file, Duration::from_secs(5)),
        "the daemon removes its own state file on the way out"
    );
    let _ = fs::remove_dir_all(&outside);
}

/// A relative `--state-file` names a file by the daemon's working directory. `leave` runs
/// somewhere else, so the daemon must report the absolute path.
#[test]
fn a_relative_state_file_is_found_from_another_directory() {
    let _serial = serial_guard();
    let workdir = std::env::temp_dir().join(format!("relative-state-{}", std::process::id()));
    fs::create_dir_all(&workdir).expect("the test temp dir is writable");
    let owner = KillOnDrop::new(
        Command::new("sleep")
            .arg("600")
            .spawn()
            .expect("a long sleep stands in for the owner process and exists on every unix host"),
    );
    let owner_pid = owner.id().to_string();
    let mut command = common::test_cmd();
    command.current_dir(&workdir);
    command.args(["create", "--name", "relative-state", "--lookup", "loopback"]);
    command.args(["--owner-pid", &owner_pid]);
    command.args(["--state-file", "daemon.json"]);
    command.stdin(Stdio::null()).stdout(Stdio::null());
    let launcher = command
        .status()
        .expect("the test binary is built before the suite runs, so its path always spawns");
    assert!(
        launcher.success(),
        "create detaches and exits zero: {launcher}"
    );
    let state_file = workdir.join("daemon.json");
    wait_ready(&state_file);

    let sessions = session_report(&owner_pid);
    assert_eq!(
        sessions["sessions"]
            .as_array()
            .expect("the JSON field is an array in this test")
            .len(),
        1,
        "a relative path in the reply names no file from this directory: {sessions}"
    );
    leave_by_owner(&owner_pid);
    assert!(
        wait_until_gone(&state_file, Duration::from_secs(5)),
        "the daemon removes its own state file on the way out"
    );
    let _ = fs::remove_dir_all(&workdir);
}

/// Stops a daemon with `SIGSTOP` and resumes it with `SIGCONT` when it goes out of scope, so a
/// failed assertion cannot leave a stopped daemon behind: it could not see its owner die.
struct StoppedDaemon(String);

impl StoppedDaemon {
    fn new(pid: String) -> Self {
        let status = Command::new("kill")
            .args(["-STOP", &pid])
            .status()
            .expect("kill is on PATH on every unix host, so it spawns");
        assert!(
            status.success(),
            "the signal reaches the daemon it was read from"
        );
        Self(pid)
    }
}

impl Drop for StoppedDaemon {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-CONT", &self.0]).status();
    }
}

/// A stopped daemon (`SIGSTOP`) still accepts a connection and never answers. The Stop hook
/// runs `session` discovery at the end of every turn, so it must not wait for such a daemon.
#[test]
fn a_stopped_daemon_does_not_hold_up_session() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("stopped-daemon");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();
    let daemon_pid = session_report(&owner_pid)["sessions"][0]["pid"].to_string();
    let _stopped = StoppedDaemon::new(daemon_pid);

    let started = Instant::now();
    let sessions = session_report(&owner_pid);
    let took = started.elapsed();

    assert!(
        took < Duration::from_secs(10),
        "a reply that never comes holds the hook for {took:?}"
    );
    assert_eq!(
        sessions["sessions"]
            .as_array()
            .expect("the JSON field is an array in this test")
            .len(),
        1,
        "the state file in the runtime base still lists the stopped daemon: {sessions}"
    );
}

/// A daemon that gives no answer in time can be alive: stopped, or slow under load. `leave` must
/// not read the silence as death, so it keeps the state file and does not claim the daemon is gone.
#[test]
fn leave_keeps_the_state_file_of_a_daemon_that_does_not_answer() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("silent-daemon");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();
    let daemon_pid = session_report(&owner_pid)["sessions"][0]["pid"].to_string();
    let _stopped = StoppedDaemon::new(daemon_pid);

    let report = leave_by_owner(&owner_pid);
    let left = report["left"]
        .as_array()
        .expect("the JSON field is an array in this test");
    assert_eq!(
        left.len(),
        1,
        "leave still reports the daemon it matched: {report}"
    );
    assert_eq!(
        left[0]["confirmed"], false,
        "nobody signalled this daemon, so it is not confirmed gone: {report}"
    );
    assert!(
        daemon.state_file.exists(),
        "a timeout says nothing about the daemon, and the file is the one it writes"
    );
}

/// A member as `session` reports it, with the files that its daemon keeps in its runtime folder.
struct MemberFiles {
    pid: String,
    lock: PathBuf,
    socket: PathBuf,
    chunks: PathBuf,
    log: PathBuf,
}

fn member_files(owner_pid: &str) -> MemberFiles {
    let report = session_report(owner_pid);
    let member = &report["sessions"][0];
    let text_of = |key: &str| {
        member[key]
            .as_str()
            .expect("a listed session carries its gossip and nickname")
            .to_owned()
    };
    let (mesh, nickname) = (text_of("gossip"), text_of("nickname"));
    let dir = default_state_file(&mesh, &nickname)
        .parent()
        .expect("a state file sits in a folder")
        .to_path_buf();
    let mesh_folder: String = mesh.chars().take(16).collect();
    let lock = common::runtime_base()
        .join("locks")
        .join(format!("{mesh_folder}-{nickname}.lock"));
    MemberFiles {
        pid: member["pid"].to_string(),
        lock,
        socket: dir.join(format!("{nickname}.ipc.sock")),
        chunks: dir.join(format!("{nickname}.chunks")),
        log: dir.join(format!("{nickname}.tracing.log")),
    }
}

/// A daemon that stops cleanly removes its own state file, socket and chunk folder before it lets go of its
/// lock, and `leave` confirms the stop when it gets that lock. So `confirmed` means that the daemon is gone
/// and has removed its files, and no command sweeps after it.
#[test]
fn leave_confirms_a_stop_only_after_the_daemon_removed_its_files() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("stopped-member");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();
    let member = member_files(&owner_pid);
    // A path that does not exist before the leave passes the `!exists()` checks below with nothing
    // removed, so each file that the daemon must remove is shown to exist first.
    assert!(
        daemon.state_file.exists(),
        "the daemon wrote its state file when it was ready"
    );
    assert!(
        member.socket.exists(),
        "the daemon made its socket at {:?}",
        member.socket
    );
    assert!(
        member.chunks.exists(),
        "the daemon made its chunk folder at {:?}",
        member.chunks
    );

    let report = leave_by_owner(&owner_pid);
    assert_eq!(
        report["left"][0]["confirmed"], true,
        "the lock is free when the daemon has ended: {report}"
    );
    assert!(
        !daemon.state_file.exists(),
        "the daemon removed its own state file before it ended"
    );
    assert!(
        !member.socket.exists(),
        "the daemon removed its own socket before it ended"
    );
    assert!(
        !member.chunks.exists(),
        "the daemon removed its own chunk folder before it ended"
    );
}

/// A daemon that was killed is not listed any more, because its record names a process that is gone. Its lock
/// file stays: no command removes a lock file, so that two starts of one member keep one lock.
#[test]
fn a_killed_daemon_is_not_listed_and_its_lock_file_stays() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("killed-member");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();
    let member = member_files(&owner_pid);
    assert!(
        member.lock.exists(),
        "a running daemon has its lock file in the registry: {:?}",
        member.lock
    );

    let status = Command::new("kill")
        .args(["-KILL", &member.pid])
        .status()
        .expect("kill is on PATH on every unix host, so it spawns");
    assert!(
        status.success(),
        "the signal reaches the daemon that the test started"
    );
    let dead_pid: u32 = member.pid.parse().expect("a pid is a number");
    let deadline = Instant::now() + Duration::from_secs(10);
    while process::is_alive(dead_pid) {
        assert!(
            Instant::now() < deadline,
            "the killed daemon is still alive"
        );
        std::thread::sleep(POLL);
    }

    let report = session_report(&owner_pid);
    let listed = report["sessions"].as_array().map_or(usize::MAX, Vec::len);
    assert_eq!(
        listed, 0,
        "a daemon that is gone is not a session: {report}"
    );
    assert!(
        member.lock.exists(),
        "the lock file of a dead member stays, with its old record"
    );
}

/// SIGKILL the daemon of `member` and wait until the process is gone. A killed daemon removes nothing itself.
fn kill_member(member: &MemberFiles) {
    let status = Command::new("kill")
        .args(["-KILL", &member.pid])
        .status()
        .expect("kill is on PATH on every unix host, so it spawns");
    assert!(
        status.success(),
        "the signal reaches the daemon that the test started"
    );
    let dead_pid: u32 = member.pid.parse().expect("a pid is a number");
    let deadline = Instant::now() + Duration::from_secs(10);
    while process::is_alive(dead_pid) {
        assert!(
            Instant::now() < deadline,
            "the killed daemon is still alive"
        );
        std::thread::sleep(POLL);
    }
    assert!(
        member.socket.exists() && member.chunks.exists(),
        "a killed daemon removes nothing itself"
    );
}

fn assert_files_exist(member: &MemberFiles, when: &str) {
    assert!(member.socket.exists(), "the socket is gone {when}");
    assert!(member.chunks.exists(), "the chunk folder is gone {when}");
}

fn assert_files_removed(member: &MemberFiles, when: &str) {
    assert!(
        !member.socket.exists(),
        "the socket of the dead member stays {when}: {}",
        member.socket.display()
    );
    assert!(
        !member.chunks.exists(),
        "the chunk folder of the dead member stays {when}: {}",
        member.chunks.display()
    );
}

/// A killed daemon cannot remove its own socket and chunk folder. Its lock is free and its record names a
/// process that is gone, so `leave` removes both.
#[test]
fn leave_removes_the_socket_and_chunks_of_a_killed_member() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("swept-member");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();
    let member = member_files(&owner_pid);
    assert_files_exist(&member, "while the daemon runs");
    kill_member(&member);

    leave_by_owner(&owner_pid);
    assert_files_removed(&member, "after leave");
}

/// `session` reads the same registry as `leave` and removes the same leftovers, and a member that
/// runs keeps both files through the `leave` of another member.
#[test]
fn a_live_member_keeps_its_files_while_leave_and_session_sweep_a_killed_one() {
    let _serial = serial_guard();
    let live = launch_detached_daemon("kept-by-sweep");
    wait_ready(&live.state_file);
    let live_files = member_files(&live.owner.id().to_string());
    let dead = launch_detached_daemon("swept-by-session");
    wait_ready(&dead.state_file);
    let dead_owner = dead.owner.id().to_string();
    let dead_files = member_files(&dead_owner);
    kill_member(&dead_files);

    session_report(&dead_owner);
    assert_files_removed(&dead_files, "after session");
    assert_files_exist(&live_files, "after the sweep of another member");
    leave_by_owner(&dead_owner);
    assert_files_exist(&live_files, "after leave of another member");
}

/// A daemon that starts holds the lock before it makes its socket, so a lock that someone holds means
/// that the member can run: nothing is removed until the lock is free.
#[test]
fn leave_removes_nothing_while_the_lock_of_the_member_is_held() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("held-lock-member");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();
    let member = member_files(&owner_pid);
    kill_member(&member);

    let lock = File::open(&member.lock).expect("the lock file of a dead member stays");
    lock.lock().expect("the lock of a dead member is free");
    leave_by_owner(&owner_pid);
    assert_files_exist(&member, "while another process holds the lock");

    drop(lock);
    leave_by_owner(&owner_pid);
    assert_files_removed(&member, "after the lock is free");
}

/// A record names a pid and the start of its process. A pid that a new process took has another start,
/// so the record is not of a running daemon.
#[test]
fn a_record_whose_pid_was_reused_is_a_dead_member() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("reused-pid-member");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();
    let member = member_files(&owner_pid);
    kill_member(&member);

    let text = fs::read_to_string(&member.lock).expect("the lock file holds the old record");
    let mut record: serde_json::Value =
        serde_json::from_str(text.trim()).expect("the record is one JSON line");
    record["pid"] = daemon.owner.id().into();
    record["start"] = 1.into();
    fs::write(&member.lock, format!("{record}\n")).expect("the lock file is writable");

    leave_by_owner(&owner_pid);
    assert_files_removed(
        &member,
        "when the pid of the record runs with another start",
    );
}

/// The mesh folder of a dead member can be a link that someone made, and the sweep must not follow it:
/// the socket and the chunk folder that sit behind the link are not the files of this member.
#[test]
fn a_mesh_folder_that_is_a_link_is_not_swept_through() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("linked-mesh-member");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();
    let member = member_files(&owner_pid);
    kill_member(&member);
    let mesh_folder = member
        .socket
        .parent()
        .expect("a socket sits in the folder of its mesh")
        .to_path_buf();
    let behind = std::env::temp_dir().join(format!("behind-the-link-{}", std::process::id()));
    let _ = fs::remove_dir_all(&behind);
    fs::rename(&mesh_folder, &behind).expect("the folder moves inside one temp dir");
    symlink(&behind, &mesh_folder).expect("the test makes the link in the runtime base");

    leave_by_owner(&owner_pid);
    let socket_behind = behind.join(member.socket.file_name().expect("a socket has a name"));
    let chunks_behind = behind.join(
        member
            .chunks
            .file_name()
            .expect("a chunk folder has a name"),
    );
    let kept = socket_behind.exists() && chunks_behind.exists();

    let _ = fs::remove_file(&mesh_folder);
    let _ = fs::remove_dir_all(&behind);
    assert!(
        kept,
        "the sweep followed the link of the mesh folder and removed what sits behind it"
    );
}

/// `confirmed` means that the daemon let go of its lock, so a start of the same mesh and nickname right after
/// `leave` finds the lock free and is not refused. There is no retry in the start and no wait in the test.
#[test]
fn a_start_right_after_leave_is_not_refused() {
    let _serial = serial_guard();
    let (anchor, anchor_log, mesh, _) = spawn_discoverable_daemon("restart-anchor");
    let dir = std::env::temp_dir().join(format!("restart-after-leave-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("the test temp dir is writable");
    let state = dir.join("node.json");
    let stderr = tmp_log("restart-after-leave");
    let spec = NodeSpec {
        mesh: &mesh,
        nickname: "restart-node",
        state: &state,
    };
    let mut first = start_keyed_node(&spec, false, &stderr);

    let mut leave = common::test_cmd();
    leave.args(["leave", "--gossip", &mesh, "--nickname", "restart-node"]);
    let out = output_within(&mut leave, QUERY_LIMIT);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("leave prints JSON");
    first.crashed = true;
    let second = start_keyed_node(&spec, false, &stderr);
    crash(second);

    assert_eq!(
        report["left"][0]["confirmed"], true,
        "leave got the lock, so the daemon had ended: {report}"
    );
    let _ = fs::remove_dir_all(&dir);
    drop(anchor);
    let _ = fs::remove_file(anchor_log);
}

/// `leave` reports the member key that the daemon had. A daemon that stops cleanly removes its state file
/// first, so a report that reads the file after the stop shows `null` for a node that had a key.
#[test]
fn leave_reports_the_member_key_of_the_daemon_it_stopped() {
    let _serial = serial_guard();
    let (anchor, anchor_log, mesh, _) = spawn_discoverable_daemon("leave-member-anchor");
    let dir = std::env::temp_dir().join(format!("leave-member-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("the test temp dir is writable");
    let state = dir.join("node.json");
    let stderr = tmp_log("leave-member");
    let mut node = start_keyed_node(
        &NodeSpec {
            mesh: &mesh,
            nickname: "leave-member-node",
            state: &state,
        },
        false,
        &stderr,
    );
    let own = ready_json(&state)["member"].clone();
    assert!(is_hex_key(&own), "the node has a member key to report");

    let mut leave = common::test_cmd();
    leave.args([
        "leave",
        "--gossip",
        &mesh,
        "--nickname",
        "leave-member-node",
    ]);
    let out = output_within(&mut leave, QUERY_LIMIT);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("leave prints JSON");
    node.crashed = true;

    assert_eq!(
        report["left"][0]["member"], own,
        "leave reports the key that the stopped daemon had: {report}"
    );
    let _ = fs::remove_dir_all(&dir);
    drop(anchor);
    let _ = fs::remove_file(anchor_log);
}

/// A second start of one mesh and nickname is refused with the message of the host, and the first daemon is
/// not harmed: it still answers on its own socket.
#[test]
fn a_second_start_of_one_member_is_refused_and_the_first_daemon_still_answers() {
    let _serial = serial_guard();
    let (anchor, anchor_log, mesh, _) = spawn_discoverable_daemon("refuse-anchor");
    let dir = std::env::temp_dir().join(format!("refuse-second-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("the test temp dir is writable");
    let first_state = dir.join("first.json");
    let stderr = tmp_log("refuse-second");
    let first = start_keyed_node(
        &NodeSpec {
            mesh: &mesh,
            nickname: "one-member",
            state: &first_state,
        },
        false,
        &stderr,
    );

    let mut second = common::test_cmd();
    second.args(["join", &mesh, "--nickname", "one-member", "--state-file"]);
    second.arg(dir.join("second.json"));
    let refused = output_within(&mut second, QUERY_LIMIT);
    let mut peers = common::test_cmd();
    peers.args(["peers", "--gossip", &mesh, "--nickname", "one-member"]);
    let answer = output_within(&mut peers, QUERY_LIMIT);
    crash(first);

    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "the second start exits non-zero: {refused:?}"
    );
    assert!(
        message.contains("another daemon already runs as one-member"),
        "the refusal is the message of the host: {message}"
    );
    assert!(
        answer.status.success(),
        "the first daemon still answers on its own socket: {answer:?}"
    );
    let _ = fs::remove_dir_all(&dir);
    drop(anchor);
    let _ = fs::remove_file(anchor_log);
}

/// Pid 0 and pid 1 are ancestors of every process, so a command that selects daemons by them
/// selects the daemons of every session. Every flag that names a session refuses them. This test
/// runs only commands that cannot stop a daemon even if the refusal were gone: the two read-only
/// commands, and `leave` with a gossip id that no daemon has. A `leave --session-pid 1` with no
/// gossip would stop every daemon on the host, and it is never run here.
#[test]
fn a_session_pid_of_init_is_refused_and_stops_nothing() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("anchor-guard");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();

    let no_such_gossip = "AbCdEf1234AbCdEf1234AbCdEf1234AbCdEf1234AbCdEf1234AbCdEf1234AbCd";
    let refused: [&[&str]; 5] = [
        &["session", "--session-pid", "1"],
        &["session", "--session-pid", "0"],
        &["bell-check", "--session-pid", "1"],
        &["leave", no_such_gossip, "--session-pid", "1"],
        &["leave", no_such_gossip, "--session-pid", "0"],
    ];
    for args in refused {
        let mut command = common::test_cmd();
        command.args(args);
        let out: std::process::Output = output_within(&mut command, QUERY_LIMIT);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && stderr.contains("init or invalid"),
            "{args:?} must fail with a clear error, and it gave {out:?}"
        );
    }

    let sessions = session_report(&owner_pid);
    assert_eq!(
        sessions["sessions"]
            .as_array()
            .expect("the JSON field is an array in this test")
            .len(),
        1,
        "the daemon that this test started is still serving: {sessions}"
    );
}

/// The kept member key of a state file, as raw bytes: the seed, then the start time of its owner (0 when the
/// key is bound to none). The tests compare bytes and never print them.
fn kept_key(state: &std::path::Path) -> Vec<u8> {
    fs::read(state.with_extension("key")).unwrap_or_default()
}

/// The start time of the owner that the key is bound to, and 0 when it is bound to none.
fn owner_start(key: &[u8]) -> u64 {
    let tail = key
        .get(32..40)
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok());
    tail.map_or(0, u64::from_le_bytes)
}

fn same_member(first: &[u8], second: &[u8]) -> bool {
    first.len() >= 32 && first.get(..32) == second.get(..32)
}

/// What names a member that a test starts: its mesh, its nickname and its state file.
struct NodeSpec<'spec> {
    mesh: &'spec str,
    nickname: &'spec str,
    state: &'spec std::path::Path,
}

/// A member that a test starts on a state file: the process that it must kill, and the daemon's pid. Behind a
/// wrapper the daemon is a child of the process that `holder` owns, so a test that panics before `crash` kills
/// the daemon here, and `crash` says that it did.
struct KeyedNode {
    holder: KillOnDrop,
    daemon_pid: u32,
    crashed: bool,
}

impl Drop for KeyedNode {
    fn drop(&mut self) {
        if !self.crashed {
            let _ = Command::new("kill")
                .args(["-KILL", &self.daemon_pid.to_string()])
                .status();
        }
    }
}

fn child_pid(parent: u32) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut command = Command::new("pgrep");
        command.args(["-P", &parent.to_string()]);
        let out = command
            .output()
            .expect("pgrep is on PATH on every unix host");
        let text = String::from_utf8_lossy(&out.stdout);
        let first = text.lines().next();
        if let Some(pid) = first.and_then(|line| line.trim().parse().ok()) {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "the wrapper never started its daemon"
        );
        std::thread::sleep(POLL);
    }
}

/// Start `join` on `state`, behind a `sh` wrapper when `wrapped`, as the e2e suite does: a new wrapper
/// is a new parent of the daemon. The stale state file of an earlier run is removed first, so that
/// `ready` waits for this run.
fn start_keyed_node(
    spec: &NodeSpec<'_>,
    wrapped: bool,
    stderr_file: &std::path::Path,
) -> KeyedNode {
    let NodeSpec {
        mesh,
        nickname,
        state,
    } = *spec;
    let _ = fs::remove_file(state);
    let stderr = File::create(stderr_file).expect("the test temp dir is writable");
    let child = if wrapped {
        // The paths are arguments of the shell, and not words of its program: a path with a space is one word.
        let mut command = Command::new("sh");
        command.args([
            "-c",
            r#""$1" --log-dir "$2" join "$3" --nickname "$4" --state-file "$5"; true"#,
            "sh",
        ]);
        command.arg(bin()).arg(common::test_log_dir());
        command.args([mesh, nickname]).arg(state);
        command.stdin(Stdio::null()).stdout(Stdio::null());
        command.stderr(stderr);
        command.spawn().expect("sh is on every unix host")
    } else {
        let mut command = common::test_cmd();
        command.args(["join", mesh, "--nickname", nickname, "--state-file"]);
        command.arg(state);
        command.stdin(Stdio::null()).stdout(Stdio::null());
        command.stderr(stderr);
        command
            .spawn()
            .expect("the test binary is built before the suite runs, so its path always spawns")
    };
    let holder = KillOnDrop::new(child);
    let daemon_pid = if wrapped {
        child_pid(holder.id())
    } else {
        holder.id()
    };
    let node = KeyedNode {
        holder,
        daemon_pid,
        crashed: false,
    };
    wait_ready(state);
    node
}

/// `SIGKILL` the daemon and its wrapper: a crash, not a leave.
fn crash(mut node: KeyedNode) {
    let status = Command::new("kill")
        .args(["-KILL", &node.daemon_pid.to_string()])
        .status()
        .expect("kill is on PATH on every unix host, so it spawns");
    node.crashed = true;
    assert!(
        status.success(),
        "the signal reaches the daemon that the test started"
    );
    let _ = node.holder.kill();
    let _ = node.holder.wait();
    std::thread::sleep(Duration::from_millis(1500));
}

/// A state file that the user chose, outside the session folder, keeps its member after a restart under
/// another parent: the e2e case, where every start has its own `sh` wrapper. No owner is bound.
#[test]
fn a_state_file_that_the_user_chose_keeps_its_member_under_another_parent() {
    let _serial = serial_guard();
    let (anchor, anchor_log, mesh, _) = spawn_discoverable_daemon("keyowner-anchor-c");
    let dir = std::env::temp_dir().join(format!("keyowner-chosen-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("the test temp dir is writable");
    let state = dir.join("node.json");
    let stderr = tmp_log("keyowner-c");

    let first = start_keyed_node(
        &NodeSpec {
            mesh: &mesh,
            nickname: "keyed-c",
            state: &state,
        },
        true,
        &stderr,
    );
    let before = kept_key(&state);
    crash(first);
    let second = start_keyed_node(
        &NodeSpec {
            mesh: &mesh,
            nickname: "keyed-c",
            state: &state,
        },
        true,
        &stderr,
    );
    let after = kept_key(&state);
    crash(second);

    assert!(
        same_member(&before, &after),
        "a restart under a new wrapper of a path that the user chose is the same member"
    );
    assert_eq!(
        owner_start(&before),
        0,
        "no owner is bound to a path that the user chose"
    );
    let _ = fs::remove_dir_all(&dir);
    drop(anchor);
    let _ = fs::remove_file(anchor_log);
}

/// The member key and the run counter are the identity of a node across a restart. `session` and `leave`
/// remove nothing of a crashed daemon: its state file stays until the same member starts again, and nothing
/// in the app may remove the key or the counter.
#[test]
fn session_and_leave_keep_the_member_key_of_a_crashed_daemon() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("key-keeper");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();
    let member = member_files(&owner_pid);
    let key = kept_key(&daemon.state_file);
    let run = fs::read(daemon.state_file.with_extension("run")).unwrap_or_default();
    assert!(
        !key.is_empty() && !run.is_empty(),
        "a running daemon keeps its key and its run counter"
    );

    let status = Command::new("kill")
        .args(["-KILL", &member.pid])
        .status()
        .expect("kill is on PATH on every unix host");
    assert!(
        status.success(),
        "the signal reaches the daemon that the test started"
    );
    std::thread::sleep(Duration::from_millis(1500));
    session_report(&owner_pid);
    leave_by_owner(&owner_pid);

    assert!(
        daemon.state_file.exists(),
        "the stale state file of the crashed daemon stays: no command cleans it"
    );
    assert_eq!(
        kept_key(&daemon.state_file),
        key,
        "the member key is still there, byte for byte"
    );
    assert_eq!(
        fs::read(daemon.state_file.with_extension("run")).unwrap_or_default(),
        run,
        "the run counter is still there, byte for byte"
    );
}

fn session_file_output(pid: &str) -> std::process::Output {
    let mut command = common::test_cmd();
    command.args(["session-file", "--session-pid", pid]);
    output_within(&mut command, QUERY_LIMIT)
}

/// The path that `session-file` prints for a pid, or a failed test when it exits non-zero.
fn session_file_path_of(pid: u32) -> PathBuf {
    let out = session_file_output(&pid.to_string());
    assert!(out.status.success(), "a live pid names a file: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "one line, and no more: {stdout:?}"
    );
    PathBuf::from(stdout.trim())
}

/// A session file is named for one process instance: the same live pid prints the same path twice, and the
/// path is in the sessions folder of the runtime base, whose folder and file the command does not make.
#[test]
fn session_file_prints_one_path_in_the_sessions_folder_for_a_live_pid() {
    // The daemon test below starts a node on the same path, because both name this process, so this test
    // must not run while that state file exists.
    let _serial = serial_guard();
    let pid = std::process::id();

    let first = session_file_path_of(pid);
    let second = session_file_path_of(pid);

    assert_eq!(first, second, "one process instance has one name");
    assert_eq!(
        first.parent(),
        Some(common::runtime_base().join("sessions").as_path()),
        "the path is where the skills keep the state file of one agent session"
    );
    assert!(!first.exists(), "the command makes no file");
}

/// Init, the pid 0 and a pid with no process name no file. The command prints nothing on stdout, so a skill line
/// that builds `--state-file` from it stops where it would have taken an empty path.
#[test]
fn session_file_refuses_init_and_a_dead_pid_and_prints_nothing() {
    let mut gone = Command::new("sleep")
        .arg("0")
        .spawn()
        .expect("sleep is on PATH on every unix host");
    let dead_pid = gone.id();
    gone.wait().expect("a gone child can be waited on");

    for pid in ["0", "1", &dead_pid.to_string()] {
        let out = session_file_output(pid);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "pid {pid} names no file: {out:?}");
        assert!(out.stdout.is_empty(), "pid {pid} printed a path: {out:?}");
        // The CLI prints an `anyhow` error with its backtrace when RUST_BACKTRACE is set, as CI does, so the
        // reason is the first line and not the only one.
        assert!(
            stderr
                .lines()
                .next()
                .is_some_and(|line| line.starts_with("Error: ")),
            "the reason is the first line on stderr: {stderr:?}"
        );
    }
}

/// Removes the files that a daemon keeps beside its state file, when it drops. A test that fails after the
/// daemon wrote them must not leave the session file of its process, which the next test of the file looks at.
/// Declare it before the node: the node is killed first, and then the files go.
struct StateFilesGuard<'path>(&'path std::path::Path);

impl Drop for StateFilesGuard<'_> {
    fn drop(&mut self) {
        for extension in ["json", "key", "run", "run.tmp", "key.lock", "run.lock"] {
            let _ = fs::remove_file(self.0.with_extension(extension));
        }
    }
}

/// The contract of the skills: a daemon that starts on the printed path is found by `ready` and by `session`.
#[test]
fn a_daemon_on_the_printed_session_file_is_found_by_ready_and_session() {
    let _serial = serial_guard();
    let (anchor, anchor_log, mesh, _) = spawn_discoverable_daemon("session-file-anchor");
    let state = session_file_path_of(std::process::id());
    // The command makes no folder, and the engine does not make the folder of a state file: the launch line of
    // the skills runs `mkdir -p` first, and a fresh CI runner has no sessions folder yet.
    fs::create_dir_all(state.parent().expect("a state file has a folder"))
        .expect("the runtime base is writable");
    let stderr = tmp_log("session-file");
    let _files = StateFilesGuard(&state);

    let node = start_keyed_node(
        &NodeSpec {
            mesh: &mesh,
            nickname: "session-file-node",
            state: &state,
        },
        false,
        &stderr,
    );
    let ready = ready_json(&state);
    let report = session_report(&std::process::id().to_string());
    let sessions = report["sessions"]
        .as_array()
        .expect("session prints a list of sessions");
    let listed = sessions
        .iter()
        .find(|row| row["nickname"] == "session-file-node");
    crash(node);

    assert_eq!(
        ready["nickname"], "session-file-node",
        "ready names the node that started on the printed path: {ready}"
    );
    assert!(
        listed.is_some(),
        "session finds a daemon that runs under this process on that path: {report}"
    );
    drop(anchor);
    let _ = fs::remove_file(anchor_log);
}

/// The JSON that `command` prints, or a failed test when it exits non-zero.
fn json_of(command: &mut Command) -> serde_json::Value {
    let out: std::process::Output = output_within(command, QUERY_LIMIT);
    assert!(out.status.success(), "the query exits zero: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).expect("a query prints one JSON document")
}

fn ready_json(state: &std::path::Path) -> serde_json::Value {
    let mut command = common::test_cmd();
    command.arg("ready").arg("--state-file").arg(state);
    json_of(&mut command)
}

fn is_hex_key(value: &serde_json::Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The roster row of `nickname` in the `peers` reply of `observer`, polled until its `member` is proved.
fn proved_row(
    mesh: &str,
    observer: &str,
    nickname: &str,
) -> (serde_json::Value, serde_json::Value) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let mut command = common::test_cmd();
        command.args(["peers", "--gossip", mesh, "--nickname", observer]);
        let reply = json_of(&mut command);
        let rows = reply["peers"].as_array().cloned().unwrap_or_default();
        let live: Vec<&serde_json::Value> = rows
            .iter()
            .filter(|row| row["nickname"] == nickname && row["quiet"] == false)
            .collect();
        if let [row] = live.as_slice()
            && is_hex_key(&row["member"])
        {
            return ((*row).clone(), reply);
        }
        assert!(
            Instant::now() < deadline,
            "the member of {nickname} was never proved to {observer}: {rows:?}"
        );
        std::thread::sleep(POLL);
    }
}

/// The member key is the stable identity of a node: `peers` shows it for every peer, and `ready`, `session`
/// and the state file show it for the node itself. `pubkey` is the key of one run and still shows beside it.
/// `member` is `null` where no proof was verified, never the old value.
#[test]
fn the_member_key_is_shown_in_peers_ready_session_and_the_state_file() {
    let _serial = serial_guard();
    let (anchor, anchor_log, mesh, anchor_nick) = spawn_discoverable_daemon("member-anchor");
    let dir = std::env::temp_dir().join(format!("member-shown-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("the test temp dir is writable");
    let state = dir.join("node.json");
    let stderr = tmp_log("member-shown");
    let node = start_keyed_node(
        &NodeSpec {
            mesh: &mesh,
            nickname: "member-node",
            state: &state,
        },
        false,
        &stderr,
    );

    let ready = ready_json(&state);
    let own = ready["member"].clone();
    assert!(
        is_hex_key(&own),
        "ready shows the member key of the node: {ready}"
    );
    assert_ne!(
        own, ready["pubkey"],
        "the member key is not the key of the run"
    );
    let written = fs::read_to_string(&state).expect("the daemon wrote its state file");
    let file: serde_json::Value = serde_json::from_str(&written)
        .expect("the daemon writes the file atomically, so a read never sees half of it");
    assert_eq!(
        file["member"], own,
        "the state file shows the same member key"
    );
    let session = session_report(&std::process::id().to_string());
    let sessions = session["sessions"]
        .as_array()
        .expect("session prints a list of sessions");
    let listed = sessions
        .iter()
        .find(|row| row["nickname"] == "member-node")
        .expect("the node is a descendant of this test, so session lists it");
    assert_eq!(listed["member"], own, "session shows the same member key");

    let (row, reply) = proved_row(&mesh, &anchor_nick, "member-node");
    assert_eq!(
        row["member"], own,
        "a peer sees the member key that the node reports"
    );
    assert_ne!(
        row["pubkey"], row["member"],
        "the row keeps the key of the run beside it"
    );
    assert!(
        is_hex_key(&reply["own_member"]),
        "peers shows the member key of the observer too: {reply}"
    );
    crash(node);
    let _ = fs::remove_dir_all(&dir);
    drop(anchor);
    let _ = fs::remove_file(anchor_log);
}

/// The scenario that failed in the e2e run: a node is killed and started again on the same state file, under a
/// new wrapper. Its member key is the same and shows once in the roster, and the key of the run is new.
#[test]
fn a_restart_on_the_same_state_file_shows_the_same_member_once() {
    let _serial = serial_guard();
    let (anchor, anchor_log, mesh, anchor_nick) = spawn_discoverable_daemon("member-restart");
    let dir = std::env::temp_dir().join(format!("member-restart-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("the test temp dir is writable");
    let state = dir.join("node.json");
    let stderr = tmp_log("member-restart");
    let spec = NodeSpec {
        mesh: &mesh,
        nickname: "member-again",
        state: &state,
    };

    let first = start_keyed_node(&spec, true, &stderr);
    let before = ready_json(&state);
    let (row_before, _) = proved_row(&mesh, &anchor_nick, "member-again");
    crash(first);
    let second = start_keyed_node(&spec, true, &stderr);
    let after = ready_json(&state);
    let key_of_run_after = after["pubkey"].clone();
    let deadline = Instant::now() + Duration::from_secs(60);
    let row_after = loop {
        let (row, _) = proved_row(&mesh, &anchor_nick, "member-again");
        if row["pubkey"] == key_of_run_after || Instant::now() >= deadline {
            break row;
        }
        std::thread::sleep(POLL);
    };
    crash(second);

    assert!(
        is_hex_key(&before["member"]),
        "ready shows a member key, so the comparison below is not null against null: {before}"
    );
    assert_eq!(
        before["member"], after["member"],
        "the node that restarts on its own state file is the same member"
    );
    assert_ne!(
        before["pubkey"], after["pubkey"],
        "the key of the run is new at every start, by design"
    );
    assert_eq!(
        row_before["member"], row_after["member"],
        "the other nodes see the same member key after the restart"
    );
    assert_eq!(
        row_after["pubkey"], key_of_run_after,
        "a row names the key of the run that started last"
    );
    let _ = fs::remove_dir_all(&dir);
    drop(anchor);
    let _ = fs::remove_file(anchor_log);
}

/// `meta get` names the member key of each slot beside the document and not in it, so that a `get`, an edit
/// and a `merge` do not write a derived value back. The slot is the key of a run, and the value comes from the
/// roster only.
#[test]
fn meta_get_names_the_member_of_each_slot_beside_the_document() {
    let _serial = serial_guard();
    let (anchor, anchor_log, mesh, anchor_nick) = spawn_discoverable_daemon("member-meta");
    let dir = std::env::temp_dir().join(format!("member-meta-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("the test temp dir is writable");
    let state = dir.join("node.json");
    let stderr = tmp_log("member-meta");
    let node = start_keyed_node(
        &NodeSpec {
            mesh: &mesh,
            nickname: "member-slot",
            state: &state,
        },
        false,
        &stderr,
    );
    let own = ready_json(&state);
    let (row, _) = proved_row(&mesh, &anchor_nick, "member-slot");

    let mut report = common::test_cmd();
    report.args([
        "meta",
        "merge",
        "--gossip",
        &mesh,
        "--nickname",
        "member-slot",
    ]);
    report.args(["--merge", "{\"status\":\"busy\"}"]);
    let merged: std::process::Output = output_within(&mut report, QUERY_LIMIT);
    assert!(
        merged.status.success(),
        "the merge of the status must end 0 before the anchor is asked: {merged:?}"
    );
    let slot = own["pubkey"]
        .as_str()
        .expect("ready shows the key of the run");
    // The reply of the merge says that the node took it, not that the anchor applied it, so the anchor is
    // asked until it shows the slot.
    let deadline = Instant::now() + Duration::from_secs(60);
    let reply = loop {
        let mut get = common::test_cmd();
        get.args(["meta", "get", "--gossip", &mesh, "--nickname", &anchor_nick]);
        let reply = json_of(&mut get);
        if !reply["members"][slot].is_null() || Instant::now() >= deadline {
            break reply;
        }
        std::thread::sleep(POLL);
    };
    assert_eq!(
        reply["members"][slot], row["member"],
        "the slot of the key of a run names the member that the roster proved: {reply}"
    );
    assert!(
        reply["document"][slot].get("member").is_none(),
        "the document itself carries no derived field"
    );
    crash(node);
    let _ = fs::remove_dir_all(&dir);
    drop(anchor);
    let _ = fs::remove_file(anchor_log);
}

/// The sweep removes only what a refused connection proves dead: a live member and a member that
/// does not answer in time keep every file.
#[test]
fn session_keeps_the_files_of_live_and_silent_members() {
    let _serial = serial_guard();
    let daemon = launch_detached_daemon("kept-member");
    wait_ready(&daemon.state_file);
    let owner_pid = daemon.owner.id().to_string();
    let member = member_files(&owner_pid);
    fs::write(&member.log, "a run's tracing log").expect("the member folder is writable");
    let all_there = |when: &str| {
        for file in [&member.socket, &member.chunks, &member.log] {
            assert!(file.exists(), "{file:?} is gone {when}");
        }
    };

    session_report(&owner_pid);
    all_there("after a session query of a live member");

    let _stopped = StoppedDaemon::new(member.pid.clone());
    session_report(&owner_pid);
    all_there("after a session query of a member that does not answer");
}

/// A caller that captures the launcher's output reads it to EOF. The detached
/// daemon must not hold the write end of that pipe, or the caller blocks until
/// the daemon exits and gets the daemon's JSON stdout as well.
#[test]
fn detached_daemon_releases_a_captured_pipe() {
    let _serial = serial_guard();
    let mut owner = KillOnDrop::new(
        Command::new("sleep")
            .arg("600")
            .spawn()
            .expect("a long sleep stands in for the owner process and exists on every unix host"),
    );
    let sessions_dir = common::runtime_base().join("sessions");
    fs::create_dir_all(&sessions_dir).expect("the test temp directory is writable");
    let state_file = sessions_dir.join(format!("{}.json", owner.id()));

    let owner_pid = owner.id().to_string();
    let mut command = common::test_cmd();
    command
        .args([
            "create",
            "--name",
            "detach-pipe",
            "--lookup",
            "loopback",
            "--owner-pid",
        ])
        .arg(owner_pid)
        .arg("--state-file")
        .arg(&state_file);
    command.stdin(Stdio::null());
    let mut launcher: Child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the test binary is built before the suite runs, so its path always spawns");
    let mut stdout = launcher
        .stdout
        .take()
        .expect("the child was spawned with a piped stdout");
    let mut stderr = launcher
        .stderr
        .take()
        .expect("the child was spawned with a piped stderr");
    let (eof_tx, eof_rx) = mpsc::channel();
    let stderr_eof = eof_tx.clone();
    std::thread::spawn(move || {
        let _ = std::io::Read::read_to_end(&mut stdout, &mut Vec::new());
        let _ = eof_tx.send(());
    });
    std::thread::spawn(move || {
        let _ = std::io::Read::read_to_end(&mut stderr, &mut Vec::new());
        let _ = stderr_eof.send(());
    });
    let both_closed = (0..2).all(|_| eof_rx.recv_timeout(Duration::from_secs(10)).is_ok());
    let launcher_status = launcher
        .wait()
        .expect("the launcher is our own child and is waited for once, so wait succeeds");

    // A failed launch starts no daemon and closes both pipes at once, so the
    // pipe check alone passes without proving anything.
    assert!(
        launcher_status.success(),
        "a failed launch starts no daemon, which would make the closed pipes prove nothing; status was {launcher_status}"
    );
    command = common::test_cmd();
    command.arg("ready").arg("--state-file").arg(&state_file);
    let ready: std::process::Output = command.output().expect("failed to run ready");
    assert!(
        ready.status.success(),
        "the ready query must succeed once the detached daemon is up, output was {ready:?}"
    );
    assert!(state_file.exists(), "no daemon wrote the state file");
    assert!(
        both_closed,
        "the detached daemon still holds the caller's stdout or stderr pipe"
    );

    let _ = owner.kill();
    let _ = owner.wait();
    assert!(
        wait_until_gone(&state_file, Duration::from_secs(15)),
        "the daemon outlived its owner"
    );
}

/// An owner of 1 (init), a dead process or a zombie would end the daemon at once or
/// never, so the start refuses all with one line on stderr.
#[test]
fn owner_pid_rejects_init_and_dead_owner() {
    let mut dead = Command::new("true")
        .spawn()
        .expect("`true` is on PATH on every unix host, so it spawns");
    let dead_pid = dead.id().to_string();
    let _ = dead.wait();
    // Exited but never reaped: a zombie still answers `kill -0`.
    let mut zombie = Command::new("true")
        .spawn()
        .expect("`true` is on PATH on every unix host, so it spawns");
    let zombie_pid = zombie.id().to_string();
    let exited = Instant::now() + Duration::from_secs(2);
    while matches!(process::start_of(zombie.id()), ProcessStart::Live(_)) {
        assert!(Instant::now() < exited, "the zombie owner never exited");
        std::thread::sleep(POLL);
    }

    for owner in ["1", dead_pid.as_str(), zombie_pid.as_str()] {
        // CI sets RUST_BACKTRACE, which makes anyhow append a backtrace to the
        // error; the one-line check is about the output a user sees by default.
        let mut command = common::test_cmd();
        command.args(["create", "--name", "owner-reject", "--lookup", "loopback"]);
        command.args(["--owner-pid", owner]);
        command
            .env_remove("RUST_BACKTRACE")
            .env_remove("RUST_LIB_BACKTRACE")
            .stdin(Stdio::null());
        let out: std::process::Output = command.output().expect("failed to run create");
        assert_eq!(
            out.status.code(),
            Some(1),
            "an owner pid that is init or dead is a usage error and exits one, owner {owner}: {out:?}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            stderr.trim().lines().count(),
            1,
            "owner {owner}: expected one stderr line, got: {stderr}"
        );
    }
    let _ = zombie.wait();
}

/// A flag error must reach the caller from the launcher itself: once the
/// launcher has exited 0, the child's error only shows as a `ready` timeout.
#[test]
fn owner_pid_launcher_reports_flag_errors() {
    let mut owner = KillOnDrop::new(
        Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("a long sleep stands in for the owner process and exists on every unix host"),
    );
    let owner_pid = owner.id().to_string();
    let mesh = habilis_network::protocol::MeshId::from("owner-flag-error").to_string();
    let bad_invocations: [&[&str]; 2] = [
        &["join", &mesh, "--name", "not-allowed"],
        &[
            "create",
            "--name",
            "flag-error",
            "--lookup",
            "loopback",
            "--transport",
            "relay",
        ],
    ];
    for args in bad_invocations {
        let mut command = common::test_cmd();
        command.args(args).args(["--owner-pid", &owner_pid]);
        let out: std::process::Output = command
            .stdin(Stdio::null())
            .output()
            .expect("failed to run the launcher");
        assert!(
            !out.status.success(),
            "the launcher hid a flag error behind exit 0 for {args:?}: {out:?}"
        );
    }
    let _ = owner.kill();
}

/// The chat messages a node has received, one per line, for an assertion message.
fn chat_debug(node: &Node) -> String {
    let lines: Vec<String> = node
        .messages()
        .map(|message| format!("{message:?}"))
        .collect();
    lines.join("\n")
}

/// The presence lines of a daemon's `--output json` stream, in the order it surfaced them: the
/// subtype, the author and the frame id.
fn presence_lines(node: &Node) -> Vec<(String, String, String)> {
    let log = node.log_contents();
    let events = log
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok());
    let presence =
        events.filter(|event| event["type"] == "presence" || event["event"] == "presence");
    presence
        .map(|event| {
            let field = |name: &str| event[name].as_str().unwrap_or_default().to_owned();
            (field("subtype"), field("author"), field("id"))
        })
        .collect()
}

/// Four peers leave at once. Each survivor sees one `left` per nick and no `joined` for that nick
/// after it, serves no frame twice, and its roster shrinks to the survivors. A guard, not a
/// reproduction: a room this small never forgets a frame, so the replay storm the engine fix
/// closed cannot start here. It holds the line the fix drew.
#[test]
fn test_mass_leave_leaves_no_ghosts() {
    let _serial = serial_guard();
    let envs = [
        ("--heal-interval-secs", "3"),
        ("--backfill-interval-secs", "2"),
    ];
    let (creator, mesh) = Node::create_args("itest", &[], &envs);
    let survivor = Node::join_flags(&mesh, "ml-survivor", &envs);
    let mut leavers: Vec<Node> = (0..4)
        .map(|index| Node::join_flags(&mesh, &format!("ml-leaver-{index}"), &envs))
        .collect();
    let all_leavers_ready = || leavers.iter().all(|leaver| leaver.wait_ready(&mesh));
    assert!(
        creator.wait_ready(&mesh) && survivor.wait_ready(&mesh) && all_leavers_ready(),
        "nodes never ready"
    );
    let count = |node: &Node, subtype: &str| {
        presence_lines(node)
            .iter()
            .filter(|(kind, _, _)| kind == subtype)
            .count()
    };
    assert!(
        wait_until(|| count(&creator, "joined"), 5, MSG_TIMEOUT) >= 5,
        "the creator never saw every joiner"
    );
    assert!(
        wait_until(|| count(&survivor, "joined"), 5, MSG_TIMEOUT) >= 5,
        "the survivor never saw the room"
    );

    for leaver in &leavers {
        leaver.sigint();
    }
    for leaver in &mut leavers {
        leaver.wait_exit();
    }
    assert!(
        wait_until(|| count(&creator, "left"), 4, MSG_TIMEOUT) >= 4,
        "the creator missed a goodbye"
    );
    assert!(
        wait_until(|| count(&survivor, "left"), 4, MSG_TIMEOUT) >= 4,
        "the survivor missed a goodbye"
    );
    // Three anti-entropy rounds: time for a resend of a departed frame to land and be dropped.
    std::thread::sleep(Duration::from_secs(3 * TEST_BACKFILL_SECS));

    for observer in [&creator, &survivor] {
        let lines = presence_lines(observer);
        for index in 0..4 {
            let nick = format!("ml-leaver-{index}");
            let lefts = lines
                .iter()
                .filter(|(kind, author, _)| kind == "left" && *author == nick)
                .count();
            assert_eq!(
                lefts, 1,
                "{nick} left {lefts} times at {}",
                observer.nickname
            );
            let last_left = lines
                .iter()
                .rposition(|(kind, author, _)| kind == "left" && *author == nick)
                .expect("one left per leaver");
            assert!(
                !lines[last_left..]
                    .iter()
                    .any(|(kind, author, _)| kind == "joined" && *author == nick),
                "{nick} came back as a ghost at {}",
                observer.nickname
            );
        }
        let total = lines.len();
        let mut ids: Vec<&str> = lines.iter().map(|(_, _, id)| id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            total,
            "a presence frame was served twice at {}",
            observer.nickname
        );
        let roster: serde_json::Value = serde_json::from_str(&cli_peers(&mesh, &observer.nickname))
            .expect("the peers command prints one JSON document on stdout and nothing else");
        assert_eq!(
            roster["peer_count"], 2,
            "ghosts in the roster of {}: {roster}",
            observer.nickname
        );
    }
}
