#![expect(
    missing_docs,
    reason = "an integration test crate has no public docs; the module path names it"
)]

use std::time::Duration;

use agent_gossip::OutputEvent;
use agent_gossip_test_fixtures as common;
use common::{InProcNode, Node, PresenceSubtype, cli_peers, cli_poll, ipc_raw, test_cmd};
use habilis_network::protocol::AuthorKey;

/// Budget for a pair to see each other at default timings.
const ROSTER_TIMEOUT: Duration = Duration::from_secs(30);

/// Silence after SIGKILL that outlives the 90 s peer timeout plus one sweep.
const KILL_SETTLE: Duration = Duration::from_secs(120);

/// Repro (b), H1: a `SIGKILL`ed peer must not come back after a fifth peer joins.
#[test]
fn killed_peer_does_not_return_after_fifth_joins() {
    let (creator, mesh) = Node::create_named("churn-kill");
    let _leave = LeaveOnDrop(mesh.clone());
    let mut victim = Node::join(&mesh, "churn-victim");
    let survivor_a = Node::join(&mesh, "churn-a");
    let survivor_b = Node::join(&mesh, "churn-b");
    std::thread::sleep(Duration::from_secs(20));

    victim.kill();
    victim.wait_exit();
    let victim_nick = victim.nickname.clone();
    std::thread::sleep(KILL_SETTLE);

    let fifth = Node::join(&mesh, "churn-fifth");
    std::thread::sleep(Duration::from_secs(30));

    let mut timeouts_of_victim = 0;
    let mut returns_of_victim = 0;
    for nick in [
        &creator.nickname,
        &survivor_a.nickname,
        &survivor_b.nickname,
    ] {
        let served = cli_poll(&mesh, nick, None);
        let events: Vec<serde_json::Value> =
            serde_json::from_str(&served).expect("poll prints JSON");
        let about_victim = |kind: &str| {
            events
                .iter()
                .filter(|event| event["event"] == kind && event["nickname"] == victim_nick.as_str())
                .count()
        };
        timeouts_of_victim += about_victim("peer_timeout");
        returns_of_victim += about_victim("peer_return");
    }
    assert!(
        timeouts_of_victim > 0,
        "victim `{victim_nick}` never timed out, so the kill was not detected"
    );

    let counts: Vec<String> = [&creator.nickname, &survivor_a.nickname, &fifth.nickname]
        .iter()
        .map(|nick| cli_peers(&mesh, nick))
        .collect();
    assert_eq!(
        returns_of_victim, 0,
        "victim `{victim_nick}` came back {returns_of_victim} time(s) after SIGKILL; peer counts: {counts:?}"
    );
}

/// Repro (a), H2: an idle pair at default timings must not time out a live peer. Six minutes
/// covers one 270 s cycle from the notes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "six-minute hold, run with --ignored"]
async fn idle_pair_holds_six_minutes() {
    let mut creator = InProcNode::create("churn-long").await;
    let mesh = creator.mesh.clone();
    let mut joiner = InProcNode::join(&mesh, "churn-long-b").await;

    assert!(
        creator
            .wait_presence_count(PresenceSubtype::Joined, 1, ROSTER_TIMEOUT)
            .await,
        "creator never saw the joiner"
    );
    assert!(
        joiner
            .wait_presence_count(PresenceSubtype::Joined, 1, ROSTER_TIMEOUT)
            .await,
        "joiner never saw the creator"
    );
    tokio::time::sleep(Duration::from_secs(360)).await;

    for (label, node) in [("creator", &mut creator), ("joiner", &mut joiner)] {
        let flaps = node
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    OutputEvent::PeerTimeout { .. } | OutputEvent::PeerReturn { .. }
                )
            })
            .count();
        assert_eq!(
            flaps, 0,
            "{label}: {flaps} peer_timeout/peer_return in six minutes"
        );
    }
}

/// Repro H4/H5: eight peers start within a few seconds; the creator sees N-1 joins, each once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn burst_start_counts_each_peer_once() {
    const PEERS: usize = 8;
    let mut creator = InProcNode::create("churn-burst").await;
    let mesh = creator.mesh.clone();
    let mut others = Vec::new();
    for index in 1..PEERS {
        others.push(InProcNode::join(&mesh, &format!("churn-burst-{index}")).await);
    }

    tokio::time::sleep(Duration::from_secs(10)).await;
    let joined_at_10 = creator.presence_count(PresenceSubtype::Joined);
    tokio::time::sleep(Duration::from_secs(50)).await;
    let joined_at_60 = creator.presence_count(PresenceSubtype::Joined);
    let joined_rows: Vec<String> = creator
        .events()
        .iter()
        .filter_map(|event| {
            let OutputEvent::Presence { message, .. } = event else {
                return None;
            };
            let is_joined = matches!(
                &message.kind,
                habilis_network::protocol::MessageKind::Presence { subtype }
                    if *subtype == PresenceSubtype::Joined
            );
            is_joined.then(|| {
                format!(
                    "author={} id={} ts={}",
                    message.author, message.id, message.timestamp
                )
            })
        })
        .collect();

    assert_eq!(
        joined_at_60,
        PEERS - 1,
        "creator saw {joined_at_60} joined events (at 10 s: {joined_at_10}): {joined_rows:#?}"
    );
}

/// Leaves the mesh on drop, so a panicking test does not leave its daemons running.
struct LeaveOnDrop(String);

impl Drop for LeaveOnDrop {
    fn drop(&mut self) {
        let _ = test_cmd().args(["leave", &self.0]).output();
    }
}

/// The nine members of the late-joiner mesh, each counting the others and itself.
const EXPECTED_PEER_COUNT: u64 = 9;

/// Repro H5: a ninth peer joins after the burst; creator and late joiner must report one `peer_count`.
#[test]
fn late_joiner_reports_same_peer_count_as_creator() {
    let (creator, mesh) = Node::create_named("churn-late");
    let _leave = LeaveOnDrop(mesh.clone());
    let mut others = Vec::new();
    for index in 1..8 {
        others.push(Node::join(&mesh, &format!("churn-late-{index}")));
    }
    std::thread::sleep(Duration::from_secs(60));

    let late = Node::join(&mesh, "churn-late-9");
    std::thread::sleep(Duration::from_secs(10));

    let peer_count = |nick: &str| -> u64 {
        let raw: serde_json::Value =
            serde_json::from_str(&cli_peers(&mesh, nick)).expect("peers prints JSON");
        raw["peer_count"]
            .as_u64()
            .expect("peers carries a numeric peer_count")
    };
    let mut counts = (0, 0);
    for _ in 0..60 {
        counts = (peer_count(&creator.nickname), peer_count(&late.nickname));
        if counts == (EXPECTED_PEER_COUNT, EXPECTED_PEER_COUNT) {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let (creator_count, late_count) = counts;
    assert_eq!(
        creator_count, EXPECTED_PEER_COUNT,
        "the creator counts the nine members, itself included: {creator_count}"
    );
    assert_eq!(
        late_count, EXPECTED_PEER_COUNT,
        "the late joiner counts the nine members, itself included: {late_count}"
    );
    assert_eq!(
        late_count, creator_count,
        "late joiner peer_count {late_count} differs from creator {creator_count}"
    );
}

/// Repro H3: a `SIGSTOP`ped daemon, held for 60 s then 200 s, must not leave a split roster.
#[test]
#[ignore = "280 s SIGSTOP run, run with --ignored"]
fn stopped_daemon_resumes_with_matching_roster() {
    let (creator, mesh) = Node::create_named("churn-stop");
    let _leave = LeaveOnDrop(mesh.clone());
    let stopped = Node::join(&mesh, "churn-stopped");
    let peer = Node::join(&mesh, "churn-peer");
    std::thread::sleep(Duration::from_secs(20));

    stopped.stop();
    std::thread::sleep(Duration::from_secs(60));
    stopped.cont();
    std::thread::sleep(Duration::from_secs(200));

    let mut timeouts = 0;
    let mut returns = 0;
    for nick in [&creator.nickname, &peer.nickname] {
        let served = cli_poll(&mesh, nick, None);
        let events: Vec<serde_json::Value> =
            serde_json::from_str(&served).expect("poll prints JSON");
        timeouts += events
            .iter()
            .filter(|event| event["event"] == "peer_timeout")
            .count();
        returns += events
            .iter()
            .filter(|event| event["event"] == "peer_return")
            .count();
    }
    let count = |nick: &str| -> u64 {
        let raw: serde_json::Value =
            serde_json::from_str(&cli_peers(&mesh, nick)).expect("peers prints JSON");
        raw["peer_count"]
            .as_u64()
            .expect("peers carries a numeric peer_count")
    };
    let count_creator = count(&creator.nickname);
    let count_stopped = count(&stopped.nickname);
    assert_eq!(
        count_creator, 3,
        "the creator counts the three members, itself included: {count_creator}"
    );
    assert_eq!(
        count_creator, count_stopped,
        "resumed daemon peer_count {count_stopped} differs from creator {count_creator}; {timeouts} peer_timeout, {returns} peer_return across survivors"
    );
}

/// Count surfaces (task B): after a `SIGKILL`ed peer goes quiet, the peer count the daemon reports
/// must equal live peers plus self, the number a user means by "online".
#[test]
#[ignore = "140 s quiet-peer guard, run with --ignored"]
fn peer_count_surfaces_count_only_live_peers_after_a_quiet_one() {
    let (creator, mesh) = Node::create_named("churn-count");
    let _leave = LeaveOnDrop(mesh.clone());
    let mut victim = Node::join(&mesh, "churn-count-victim");
    let survivor = Node::join(&mesh, "churn-count-survivor");
    std::thread::sleep(Duration::from_secs(20));

    victim.kill();
    victim.wait_exit();
    std::thread::sleep(Duration::from_secs(120));

    let roster: serde_json::Value =
        serde_json::from_str(&cli_peers(&mesh, &creator.nickname)).expect("peers prints JSON");
    let roster_count = roster["peer_count"]
        .as_u64()
        .expect("peers carries peer_count");
    let peers: &Vec<serde_json::Value> = roster["peers"]
        .as_array()
        .expect("peers carries a peers array");
    let live_entries = peers.iter().filter(|entry| entry["quiet"] == false).count();
    let live_plus_self = live_entries as u64 + 1;

    let info_line = r#"{"command":"info"}"#;
    let info: serde_json::Value =
        serde_json::from_str(&ipc_raw(&mesh, &creator.nickname, info_line))
            .expect("info prints JSON");
    let info_peer_count: &serde_json::Value = &info["peer_count"];
    let info_count: u64 = info_peer_count.as_u64().expect("info carries peer_count");

    assert_eq!(
        (roster_count, info_count),
        (live_plus_self, live_plus_self),
        "peer_count counts the quiet victim: peers says {roster_count}, info says {info_count}, live plus self is {live_plus_self} (survivor {})",
        survivor.nickname
    );

    let doctor = test_cmd()
        .args(["doctor", "--no-probe", "--output", "json"])
        .output()
        .expect("doctor runs");
    let doctor_out = String::from_utf8_lossy(&doctor.stdout).to_string();
    let doctor_count = doctor_members(&doctor_out, &creator.nickname)
        .expect("doctor reports the creator's member count");
    assert_eq!(
        doctor_count, live_plus_self,
        "doctor says {doctor_count} members, live plus self is {live_plus_self}"
    );
}

/// The member count doctor prints after `<nick> · `, from its JSON or human text.
fn doctor_members(out: &str, nick: &str) -> Option<u64> {
    let key = format!("<{nick}>");
    let rest = &out[out.find(&key)? + key.len()..];
    let after_dot = rest.split_once('·')?.1;
    after_dot.split_whitespace().next()?.parse().ok()
}

/// The `(key, quiet)` of each row of `observer` that names `nickname`.
async fn rows_named(observer: &InProcNode, nickname: &str) -> Vec<(AuthorKey, bool)> {
    let roster = observer
        .session
        .peers()
        .await
        .expect("the session of the observer stays up for the whole test");
    let named = roster
        .peers
        .iter()
        .filter(|entry| entry.nickname.as_str() == nickname);
    named.map(|entry| (entry.key, entry.quiet)).collect()
}

/// Wait until `node` captures the `nickname_held` event: the proof that it is a member of its own that
/// claims a nickname that another member holds, and not a second run of the member that holds it. The
/// line that `--output json` prints for the event is pinned by the unit tests in `emission/output.rs`.
async fn await_nickname_held(node: &mut InProcNode) {
    let held = node
        .wait_for(Duration::from_secs(60), |events| {
            events
                .iter()
                .any(|event| matches!(event, OutputEvent::NicknameHeld { .. }))
        })
        .await;
    assert!(
        held,
        "the node never reported nickname_held, so it is not a second member of the nickname"
    );
}

/// Wait until `observer` sees the holder of `nickname`, and check that it sees one, not two: a
/// second key under a held nickname is not admitted, so the roster never holds the name twice.
async fn await_the_single_holder(observer: &InProcNode, nickname: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while rows_named(observer, nickname).await.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the observer never saw the holder of {nickname}"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(
        rows_named(observer, nickname).await.len(),
        1,
        "the observer sees a second member under {nickname}: the gate admitted a second key"
    );
}

/// An observer that joins after two members of one nickname are known to each other holds one row for the
/// nickname, not two: the nickname gate admits one member, and a session that was admitted before the proof of
/// the holder came goes out when that proof binds the nickname. (BEFORE the eviction at bind, the late observer
/// held both rows for the 90 s of the alive timeout, and then one quiet row stayed listed.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_observer_holds_one_row_for_a_nickname_of_two_members() {
    let creator = InProcNode::create("churn-late").await;
    let _first = InProcNode::join(&creator.mesh, "churn-late-dup").await;
    let mut second = InProcNode::join(&creator.mesh, "churn-late-dup").await;
    await_nickname_held(&mut second).await;
    let observer = InProcNode::join(&creator.mesh, "churn-late-obs").await;

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut rows = rows_named(&observer, "churn-late-dup").await.len();
    while rows != 1 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(1)).await;
        rows = rows_named(&observer, "churn-late-dup").await.len();
    }
    assert_eq!(
        rows, 1,
        "the late observer holds {rows} rows for a nickname that one member holds"
    );
}

/// A member that the nickname gate kept out of a late observer is not out for good: when the member that holds
/// the nickname leaves, the other member is a live peer of the observer again, whatever the order of the proofs
/// was. (An evicted honest session was once recorded as one that said goodbye, and stayed out.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_evicted_member_is_a_peer_again_when_the_holder_leaves() {
    let creator = InProcNode::create("churn-return").await;
    let nickname = "churn-return-dup";
    let first = InProcNode::join(&creator.mesh, nickname).await;
    let mut second = InProcNode::join(&creator.mesh, nickname).await;
    await_nickname_held(&mut second).await;
    let observer = InProcNode::join(&creator.mesh, "churn-return-obs").await;

    // The observer admits one holder: the member whose proof it takes first.
    let settle = std::time::Instant::now() + Duration::from_secs(100);
    let mut rows = rows_named(&observer, nickname).await;
    while rows.len() != 1 && std::time::Instant::now() < settle {
        tokio::time::sleep(Duration::from_secs(1)).await;
        rows = rows_named(&observer, nickname).await;
    }
    assert_eq!(
        rows.len(),
        1,
        "the observer never settled on one holder: {rows:?}"
    );
    let holder_key = rows[0].0;
    let first_key = first
        .session
        .own_key()
        .await
        .expect("the session of the first member stays up");
    let (holder, evicted) = if first_key == holder_key {
        (first, second)
    } else {
        (second, first)
    };
    let evicted_key = evicted
        .session
        .own_key()
        .await
        .expect("the session of the evicted member stays up");
    assert_ne!(
        evicted_key, holder_key,
        "the two members share one session key, so one of them is not a member of its own"
    );

    holder.leave().await;

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut back = false;
    while !back && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(1)).await;
        back = rows_named(&observer, nickname)
            .await
            .iter()
            .any(|(key, quiet)| *key == evicted_key && !quiet);
    }
    assert!(
        back,
        "the member that the gate kept out is not a live peer of the observer after the holder left: {:?}",
        rows_named(&observer, nickname).await
    );
}

/// BEFORE: two daemons under one nickname were both in the roster, and a message to the nickname was
/// refused as ambiguous. The nickname gate now admits one key for a nickname, so the roster holds the
/// name once. AFTER: the observer sees one holder and a message to the nickname reaches it. (The test
/// belongs to the churn work merged in main, #76.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_to_a_nickname_of_two_daemons_reaches_the_one_holder() {
    let creator = InProcNode::create("churn-amb").await;
    let _first = InProcNode::join(&creator.mesh, "churn-amb-dup").await;
    let mut second = InProcNode::join(&creator.mesh, "churn-amb-dup").await;
    await_nickname_held(&mut second).await;
    let observer = InProcNode::join(&creator.mesh, "churn-amb-obs").await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    await_the_single_holder(&observer, "churn-amb-dup").await;

    observer.await_peer_card("churn-amb-dup").await;
    let sent = observer.try_msg("churn-amb-dup", "to-the-one-holder").await;
    assert!(
        sent.is_ok(),
        "a message to a nickname with one holder was refused: {sent:?}"
    );
}

/// BEFORE: a task to an ambiguous nickname was refused, not parked on one of its holders. AFTER: the
/// nickname has one holder, and the task goes to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_task_to_a_nickname_of_two_daemons_reaches_the_one_holder() {
    let creator = InProcNode::create("churn-task").await;
    let _first = InProcNode::join(&creator.mesh, "churn-task-dup").await;
    let mut second = InProcNode::join(&creator.mesh, "churn-task-dup").await;
    await_nickname_held(&mut second).await;
    let observer = InProcNode::join(&creator.mesh, "churn-task-obs").await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    await_the_single_holder(&observer, "churn-task-dup").await;

    let created = observer.create_task("churn-task-dup", "t").await;
    assert!(
        created.get("result").is_some(),
        "a task to a nickname with one holder was refused: {created}"
    );
}
