//! Topic split-brain regression, in-process over the real event loop + iroh
//! mesh (`InProcNode`): two members join the same topic **simultaneously**, so
//! both probe the shared seed-derived rendezvous before either has bound it,
//! and both claim the beacon — the split that used to be permanent (each
//! same-id co-host captures its own bootstrap dial, and nothing re-probed for
//! a rival once a beacon was held). The periodic rival re-check shed
//! (`shed_rival_beacon_if_due`) must merge them.
//!
//! Own integration binary: the process-global `Tuning` is a `OnceLock`, and
//! this suite needs the shed cadences collapsed to seconds plus the
//! `topic_mdns_only` lane (public-shaped rendezvous, LAN-only reach — CI has
//! no public relay, and the narrowed config also derives a topic id no
//! production peer on the same string can land in).

use std::time::{Duration, Instant};

use agent_gossip_test_fixtures as common;
use agent_habilis_mesh::util::tuning::{Tuning, init};

use common::{InProcNode, MSG_TIMEOUT};

/// Serialize the two tests (the fixtures' `serial_guard` is a sync guard that
/// can't be held across `await`): each spawns real mDNS responders and probes
/// with second-scale budgets, and running them concurrently on one host
/// starves both convergences past their deadlines.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn init_short_tuning() {
    // Surface the daemon's tracing under `RUST_LOG=…` when debugging a
    // convergence failure; a no-op (Err) when a subscriber is already set.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    init(Tuning {
        // Probe budget self-clamps to min(HEAL_PROBE_SECS, heal_interval);
        // keep it at the full 5s — an mDNS resolve of the rendezvous id
        // takes a query round, and a too-short probe re-binds a duplicate.
        heal_interval_secs: 5,
        rival_recheck_first_secs: 3,
        rival_recheck_secs: 8,
        rival_recheck_meshed_secs: 12,
        topic_mdns_only: true,
        ..Tuning::DEFAULTS
    });
}

/// `await_mutual_cards` with an explicit deadline instead of the fixture's
/// fixed `MSG_TIMEOUT` — split-merge convergence spans several shed rounds.
async fn await_cards_bounded(left: &InProcNode, right: &InProcNode, deadline: Duration) {
    let until = Instant::now() + deadline;
    let left_sees = format!("/peers/{}/card", right.nickname);
    let right_sees = format!("/peers/{}/card", left.nickname);
    loop {
        let crossed = left.meta_get().await.pointer(&left_sees).is_some()
            && right.meta_get().await.pointer(&right_sees).is_some();
        if crossed {
            return;
        }
        assert!(
            Instant::now() < until,
            "the two topic joiners never merged within {deadline:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// A topic string no other run (or parallel CI runner on the same LAN — mDNS
/// crosses process and machine boundaries) can collide with.
fn unique_topic() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    format!("topic-split-{}-{nanos}", std::process::id())
}

/// Control for the lane itself: a *staggered* second joiner takes the
/// no-race path (its startup probe finds the first joiner's live beacon and
/// yields), so this converging proves mDNS resolution works in-process —
/// isolating any simultaneous-test failure to the shed/merge logic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn staggered_topic_joiners_converge() {
    let _serial = SERIAL.lock().await;
    init_short_tuning();
    let topic = unique_topic();

    let alice = InProcNode::topic(&topic, "stag-alice").await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let bob = InProcNode::topic(&topic, "stag-bob").await;
    assert_eq!(alice.mesh, bob.mesh);

    // Same bound as the simultaneous test, not the fixture's fixed 60s
    // helper: when host mDNS is slow, even the no-race path may fall back
    // to a duplicate claim repaired by shed rounds.
    await_cards_bounded(&alice, &bob, Duration::from_secs(150)).await;

    alice.leave().await;
    bob.leave().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_simultaneous_topic_joiners_converge() {
    let _serial = SERIAL.lock().await;
    init_short_tuning();
    let topic = unique_topic();

    // Join in the same instant so both startup probes run before either
    // beacon exists — the maximal-race shape of the field failure (two agents
    // told to meet on a topic start within seconds of each other).
    let (mut alice, mut bob) = tokio::join!(
        InProcNode::topic(&topic, "topic-alice"),
        InProcNode::topic(&topic, "topic-bob"),
    );
    assert_eq!(
        alice.mesh, bob.mesh,
        "same string must derive the same room id"
    );

    // Convergence = each side holds the other's card in its meta doc (the
    // 2-peer roster + card-exchange marker). Pre-fix this hangs forever:
    // both nodes sit alone in their own overlay and the cards never cross.
    // Merge timing is a geometric race (each shed round separates the two
    // holders' phases by fresh jitter until one probe lands while the other
    // still holds), so the bound is several rounds, not one fixture timeout.
    // Generous for a loaded host (the full suite runs other binaries in
    // parallel and a busy machine stretches every mDNS query round); a
    // quiet run converges in ~15-25s.
    await_cards_bounded(&alice, &bob, Duration::from_secs(150)).await;

    // And traffic flows across the merged overlay, both directions.
    alice.send("after-merge-a").await;
    assert!(
        bob.wait_body("after-merge-a", MSG_TIMEOUT).await,
        "bob never saw alice's post-merge broadcast"
    );
    bob.send("after-merge-b").await;
    assert!(
        alice.wait_body("after-merge-b", MSG_TIMEOUT).await,
        "alice never saw bob's post-merge broadcast"
    );

    alice.leave().await;
    bob.leave().await;
}
