//! Local-stability hardening: a healthy quiet mesh should *stay* healthy.
//!
//! The reliability suite in `gossip_network.rs` covers recovery from
//! disruption (SIGKILL beacon migration, SIGSTOP sleep/wake heal,
//! anti-entropy backfill). This file covers the **negative-space**
//! assertions — that *nothing goes wrong* in a quiet, healthy mesh —
//! which is exactly the class of regression a future iroh / gossip
//! change could silently introduce.
//!
//! All in-process via the `InProcNode` harness (real iroh mesh, sub-second
//! to ~30s each, deterministic). Total CI cost ~70s.

use agent_gossip_test_fixtures as common;

use std::time::Duration;

use agent_gossip::OutputEvent;
use common::InProcNode;

/// How long the steady-state test holds the mesh quiet before asserting
/// nothing spurious happened. Long enough to outlive a fresh-mesh
/// settling burst; short enough to keep CI fast.
const STEADY_HOLD: Duration = Duration::from_secs(30);

/// Budget for a broadcast to fan out to every member of a small mesh.
const FANOUT_WAIT: Duration = Duration::from_secs(10);

/// Budget for a node's view of the roster to converge (see N−1 `joined`
/// presence events). Generous so a slow CI box doesn't flake.
const ROSTER_TIMEOUT: Duration = Duration::from_secs(15);

/// **Steady-state hold (the most valuable test).** A 3-node mesh meshes,
/// then sits idle for 30s. Asserts no spurious `peer_timeout` or
/// `presence:left` surfaced anywhere (all nodes were alive throughout)
/// **and** a fresh broadcast at the end still fans out to every member —
/// the latter is the durable proof that the gossip overlay survived the
/// quiet period without silently degrading.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mesh_stays_meshed_no_spurious_flaps() {
    let mut creator = InProcNode::create("stab-steady").await;
    let mesh = creator.mesh.clone();
    let mut joiner_a = InProcNode::join(&mesh, "stab-a").await;
    let mut joiner_b = InProcNode::join(&mesh, "stab-b").await;

    // Each node converges on the full roster.
    assert!(
        creator.wait_presence_count(true, 2, ROSTER_TIMEOUT).await,
        "creator never surfaced both joiners"
    );
    assert!(
        joiner_a.wait_presence_count(true, 2, ROSTER_TIMEOUT).await,
        "joiner a never saw both other peers"
    );
    assert!(
        joiner_b.wait_presence_count(true, 2, ROSTER_TIMEOUT).await,
        "joiner b never saw both other peers"
    );

    tokio::time::sleep(STEADY_HOLD).await;

    // Negative space: nothing flapped while every node was alive.
    for (label, node) in [
        ("creator", &mut creator),
        ("stab-a", &mut joiner_a),
        ("stab-b", &mut joiner_b),
    ] {
        let timeouts = node
            .events()
            .iter()
            .filter(|event| matches!(event, OutputEvent::PeerTimeout { .. }))
            .count();
        let lefts = node.presence_count(false);
        assert_eq!(
            timeouts, 0,
            "{label} surfaced a spurious peer_timeout during the quiet hold (all peers were alive)"
        );
        assert_eq!(
            lefts, 0,
            "{label} surfaced a spurious presence:left during the quiet hold (no peer departed)"
        );
    }

    // The overlay must still deliver after the hold — durable proof the
    // mesh did not silently degrade.
    let _ = creator.send("steady-probe").await;
    assert!(
        joiner_a.wait_body("steady-probe", FANOUT_WAIT).await,
        "stab-a never received the late broadcast — mesh degraded silently"
    );
    assert!(
        joiner_b.wait_body("steady-probe", FANOUT_WAIT).await,
        "stab-b never received the late broadcast — mesh degraded silently"
    );
}

/// **Concurrent joins.** Five joiners launched simultaneously must all
/// bootstrap, mesh, and receive a creator broadcast. Catches loopback
/// rendezvous-ladder and `BEACON_COHOST_GRACE` contention regressions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_joins_all_mesh_and_receive() {
    let mut creator = InProcNode::create("stab-concurrent").await;
    let mesh = creator.mesh.clone();
    // tokio::join! polls these 5 join futures cooperatively in one task,
    // so they race for the rendezvous together rather than each waiting
    // for the previous to finish.
    let (n1, n2, n3, n4, n5) = tokio::join!(
        InProcNode::join(&mesh, "cn1"),
        InProcNode::join(&mesh, "cn2"),
        InProcNode::join(&mesh, "cn3"),
        InProcNode::join(&mesh, "cn4"),
        InProcNode::join(&mesh, "cn5"),
    );
    let mut joiners = [n1, n2, n3, n4, n5];

    assert!(
        creator.wait_presence_count(true, 5, ROSTER_TIMEOUT).await,
        "creator did not surface all 5 concurrent joiners as `joined`"
    );

    let _ = creator.send("concurrent-probe").await;
    for joiner in &mut joiners {
        let nick = joiner.nickname.clone();
        assert!(
            joiner.wait_body("concurrent-probe", FANOUT_WAIT).await,
            "joiner {nick} never received the broadcast"
        );
    }
}

/// **Fan-out completeness from every origin.** In a 4-node mesh, *every*
/// node broadcasts a unique body and *every other* node receives all the
/// others' broadcasts. A single-origin test would miss asymmetric overlay
/// defects (e.g. only the creator can reach all peers).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fanout_complete_from_each_origin() {
    let mut creator = InProcNode::create("stab-fanout").await;
    let mesh = creator.mesh.clone();
    let mut j1 = InProcNode::join(&mesh, "fo-j1").await;
    let mut j2 = InProcNode::join(&mesh, "fo-j2").await;
    let mut j3 = InProcNode::join(&mesh, "fo-j3").await;

    for (label, node) in [
        ("creator", &mut creator),
        ("fo-j1", &mut j1),
        ("fo-j2", &mut j2),
        ("fo-j3", &mut j3),
    ] {
        assert!(
            node.wait_presence_count(true, 3, ROSTER_TIMEOUT).await,
            "{label} did not see all 3 other peers as `joined`"
        );
    }

    let _ = creator.send("from-creator").await;
    let _ = j1.send("from-j1").await;
    let _ = j2.send("from-j2").await;
    let _ = j3.send("from-j3").await;

    // Each node receives every body authored by another node.
    for (label, node, expect) in [
        ("creator", &mut creator, ["from-j1", "from-j2", "from-j3"]),
        ("fo-j1", &mut j1, ["from-creator", "from-j2", "from-j3"]),
        ("fo-j2", &mut j2, ["from-creator", "from-j1", "from-j3"]),
        ("fo-j3", &mut j3, ["from-creator", "from-j1", "from-j2"]),
    ] {
        for body in expect {
            assert!(
                node.wait_body(body, FANOUT_WAIT).await,
                "{label} did not receive `{body}` from its origin"
            );
        }
    }
}

/// **Roster convergence.** Every node's view of the participant set
/// reaches the full N within a budget — each node surfaces N−1 `joined`
/// presence events for the other peers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roster_converges_to_4() {
    let mut creator = InProcNode::create("stab-roster").await;
    let mesh = creator.mesh.clone();
    let mut j1 = InProcNode::join(&mesh, "ro-j1").await;
    let mut j2 = InProcNode::join(&mesh, "ro-j2").await;
    let mut j3 = InProcNode::join(&mesh, "ro-j3").await;

    for (label, node) in [
        ("creator", &mut creator),
        ("ro-j1", &mut j1),
        ("ro-j2", &mut j2),
        ("ro-j3", &mut j3),
    ] {
        assert!(
            node.wait_presence_count(true, 3, ROSTER_TIMEOUT).await,
            "{label} did not converge to 3 peer `joined` presences within {ROSTER_TIMEOUT:?}"
        );
    }
}

/// **Above-the-old-cap full mesh stays churn-free.** An 8-node mesh exceeds
/// iroh-gossip's default active-view capacity of 5 — pre-fix this size formed a
/// *partial* mesh and churned (the membership-maintenance leak driver). With the
/// raised `GOSSIP_ACTIVE_VIEW_CAPACITY` (64) it forms a **full mesh** instead, so
/// it must hold steady with **zero** spurious `peer_timeout`/`left` over a quiet
/// hold and still fan a late broadcast out to all 7 peers. This is the
/// regression guard that the active-view raise took effect: revert the const to
/// 5 and this test churns/fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn above_old_cap_mesh_stays_full_mesh() {
    const JOINERS: usize = 7; // 8 nodes total — > old cap (5), ≤ new cap (64)
    let mut creator = InProcNode::create("stab-cap").await;
    let mesh = creator.mesh.clone();
    let mut joiners = Vec::with_capacity(JOINERS);
    for index in 0..JOINERS {
        joiners.push(InProcNode::join(&mesh, &format!("cap-j{index}")).await);
    }

    // All 8 nodes converge on the full roster (7 peers each).
    assert!(
        creator
            .wait_presence_count(true, JOINERS, ROSTER_TIMEOUT)
            .await,
        "creator never surfaced all {JOINERS} joiners"
    );
    for joiner in &mut joiners {
        let nick = joiner.nickname.clone();
        assert!(
            joiner
                .wait_presence_count(true, JOINERS, ROSTER_TIMEOUT)
                .await,
            "{nick} never saw all {JOINERS} peers"
        );
    }

    tokio::time::sleep(STEADY_HOLD).await;

    // Zero churn while every node was alive — the full mesh has nothing to
    // shuffle, so no promote/demote flaps.
    for joiner in &mut joiners {
        let nick = joiner.nickname.clone();
        let timeouts = joiner
            .events()
            .iter()
            .filter(|event| matches!(event, OutputEvent::PeerTimeout { .. }))
            .count();
        assert_eq!(
            timeouts, 0,
            "{nick} surfaced a spurious peer_timeout — the >5-node mesh is churning (active-view raise regressed?)"
        );
        assert_eq!(
            joiner.presence_count(false),
            0,
            "{nick} surfaced a spurious presence:left during the quiet hold"
        );
    }

    // Overlay still delivers to every peer after the hold.
    let _ = creator.send("cap-probe").await;
    for joiner in &mut joiners {
        let nick = joiner.nickname.clone();
        assert!(
            joiner.wait_body("cap-probe", FANOUT_WAIT).await,
            "{nick} never received the late broadcast — mesh degraded"
        );
    }
}
