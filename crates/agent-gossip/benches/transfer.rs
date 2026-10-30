//! Two-node loopback transfer soak. Streams a large **deterministic**
//! payload from one in-process peer to another over the real gossip mesh
//! (loopback only — fully offline), verifies every delivered chunk
//! byte-exact on the receiver, and prints a throughput report.
//!
//! It is a plain `fn main()` harness (`harness = false`), not divan: a
//! single long async transfer has nothing for a sampling framework to do.
//! Uses only the public `api` surface.
//!
//! Run: `cargo task bench transfer` (or `cargo bench --bench transfer`).
//! Size is env-tunable: `BENCH_TRANSFER_MB=2 cargo task bench transfer`
//! for a quick smoke; default 100 MB.
//!
//! Why deterministic-and-verified rather than asserting 100% delivery:
//! the gossip path is best-effort and the message log only keeps the most
//! recent ~200 ids, so across tens of thousands of messages anti-entropy
//! cannot backfill an early drop. We therefore verify each chunk that
//! *does* arrive and report the delivery ratio (≈100% on loopback).
use std::time::{Duration, Instant};

use agent_gossip::api::{CreateConfig, JoinConfig, MeshSession};
use agent_gossip::{JoinTarget, MeshName, MessageBody};
use habilis_network::protocol::messages::Authored;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::{self};
use tokio::sync::watch;

/// Zero-padded index width in the `"{index:0N}:"` body prefix — one shared
/// source for the format width and the receiver's parse.
const INDEX_DIGITS: usize = 8;
/// `"{index:08}:"` — the index digits plus the `:` separator.
const PREFIX_LEN: usize = INDEX_DIGITS + 1;
/// Payload bytes per chunk, before the prefix.
const PAYLOAD_LEN: usize = 3000;
const CHUNK_BODY_LEN: usize = PREFIX_LEN + PAYLOAD_LEN;
/// Printable-ASCII range the payload draws from: `33..=126` (94 glyphs).
const ASCII_LO: u8 = 33;
const ASCII_SPAN: u64 = 94;

// A chunk must fit one gossip message once the JSON envelope is added.
// Tie the chunk size to the live wire cap so shrinking `MAX_MESSAGE_SIZE`
// fails the build here instead of silently dropping every send.
const _: () = assert!(
    CHUNK_BODY_LEN + 512 <= agent_gossip::MAX_MESSAGE_SIZE,
    "chunk body leaves too little headroom under MAX_MESSAGE_SIZE for the JSON envelope"
);

/// Deterministic printable-ASCII payload for `index` (`SplitMix64` →
/// `33..=126`). Pure function of the index, so the receiver regenerates
/// the expected bytes independently — no shared state, order-agnostic.
fn payload_for(index: u32) -> String {
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ u64::from(index).wrapping_mul(0x2545_f491_4f6c_dd1d);
    let mut out = String::with_capacity(PAYLOAD_LEN);
    for _ in 0..PAYLOAD_LEN {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^= mixed >> 31;
        out.push(char::from(
            ASCII_LO + u8::try_from(mixed % ASCII_SPAN).expect("< 94 fits u8"),
        ));
    }
    out
}

fn body_for(index: u32) -> String {
    let payload = payload_for(index);
    format!("{index:0INDEX_DIGITS$}:{payload}")
}

/// Parse a received body as a chunk, returning its verified index, or
/// `None` for any non-chunk traffic (presence/alive) or a malformed body.
/// Panics on a chunk whose payload does not match — that is data
/// corruption on the wire, a real bug, not a soft delivery miss.
fn verify_chunk(body: &str, expected_count: u32) -> Option<u32> {
    let (idx_str, payload) = body.split_once(':')?;
    if idx_str.len() != INDEX_DIGITS {
        return None;
    }
    let index: u32 = idx_str.parse().ok()?;
    if index >= expected_count {
        return None;
    }
    assert!(
        payload == payload_for(index),
        "chunk {index} arrived corrupted: payload mismatch"
    );
    Some(index)
}

/// Lossless `u64` to `f64` for values under 2^53, built from two `u32` halves.
fn exact_f64(value: u64) -> f64 {
    let high = u32::try_from(value >> 32).expect("a u64 shifted by 32 fits u32");
    let low = u32::try_from(value & u64::from(u32::MAX)).expect("masked to 32 bits");
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

/// What the receiver's spawned task has seen so far. The task owns the counts
/// and publishes them; the main task only reads the latest value.
#[derive(Default)]
struct DeliveryTally {
    delivered: usize,
    delivered_bytes: u64,
    lagged: u64,
}

/// Drain `rx` until `chunk_count` distinct chunk indices have arrived (or the
/// channel closes), tallying delivered count/bytes and any broadcast lag
/// through `tally`. Body of the receiver's spawned task.
async fn receive_chunks(
    mut rx: broadcast::Receiver<Authored>,
    chunk_count: u32,
    tally: watch::Sender<DeliveryTally>,
) {
    let mut seen = vec![false; chunk_count as usize];
    loop {
        let message = match rx.recv().await {
            Ok(message) => message,
            Err(RecvError::Lagged(skipped)) => {
                tally.send_modify(|tally| tally.lagged += skipped);
                continue;
            }
            Err(RecvError::Closed) => break,
        };
        let Some(index) = verify_chunk(message.body.as_str(), chunk_count) else {
            continue;
        };
        let slot = &mut seen[index as usize];
        if *slot {
            continue;
        }
        *slot = true;
        let mut done = false;
        tally.send_modify(|tally| {
            tally.delivered_bytes += message.body.as_str().len() as u64;
            tally.delivered += 1;
            done = tally.delivered == chunk_count as usize;
        });
        if done {
            break;
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let raw_mb: Option<String> = std::env::var("BENCH_TRANSFER_MB").ok();
    let target_mb: u64 = raw_mb.and_then(|raw| raw.parse().ok()).unwrap_or(100);
    let target_bytes = target_mb * 1024 * 1024;
    let chunk_count: u32 =
        u32::try_from((target_bytes / CHUNK_BODY_LEN as u64).max(1)).expect("chunk count fits u32");

    eprintln!("transfer bench: {target_mb} MB → {chunk_count} chunks of {CHUNK_BODY_LEN} B");

    // Node A creates a loopback mesh.
    let create_cfg = CreateConfig::new(MeshName::try_new("bench").expect("valid name"));
    let node_a = MeshSession::create(create_cfg).await.expect("create");

    let target: JoinTarget = node_a
        .mesh_id()
        .as_str()
        .parse()
        .expect("a freshly minted id parses");
    let node_b = MeshSession::join(JoinConfig::new(target))
        .await
        .expect("join");

    // Subscribe before sending so we see everything sent after this point.
    let rx = node_b.messages();

    let (tally_tx, tally_rx) = watch::channel(DeliveryTally::default());

    // Receiver: verify + tally distinct chunk indices. Owns its `seen`
    // bitset; reports progress through the shared atomics.
    let receiver = tokio::spawn(receive_chunks(rx, chunk_count, tally_tx));

    // Pre-build every chunk body *before* the clock starts: payload
    // generation + `MessageBody` validation are not transfer work and must
    // not inflate the throughput number. (The receiver still regenerates
    // independently to verify — only the sender is inside the timed window.)
    let bodies: Vec<MessageBody> = (0..chunk_count)
        .map(|index| MessageBody::try_new(&body_for(index)).expect("valid body"))
        .collect();

    // Warm up the mesh: resend chunk 0 until the receiver sees it (deduped
    // by index), so the timed window measures steady-state transfer, not
    // rendezvous/bootstrap latency.
    let warmup_deadline = Instant::now() + Duration::from_secs(20);
    while tally_rx.borrow().delivered == 0 {
        assert!(
            Instant::now() < warmup_deadline,
            "mesh never formed: node B received no chunk within 20s"
        );
        let _ = node_a.broadcast(bodies[0].clone()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Timed bulk send — pure send round-trips, no generation. Resending
    // index 0 (warmup) is harmless: the receiver dedups by index.
    let started = Instant::now();
    for body in bodies {
        let _ = node_a.broadcast(body).await;
    }

    // Wait for drain: done at N, or give up after a no-progress stall /
    // overall cap (a soft delivery miss, reported as ratio < 100%).
    let overall_deadline = started + Duration::from_secs(60 + target_mb * 3);
    let mut last_seen = tally_rx.borrow().delivered;
    let mut last_progress = Instant::now();
    loop {
        let now_delivered = tally_rx.borrow().delivered;
        if now_delivered >= chunk_count as usize || Instant::now() > overall_deadline {
            break;
        }
        if now_delivered != last_seen {
            last_seen = now_delivered;
            last_progress = Instant::now();
        } else if last_progress.elapsed() > Duration::from_secs(5) {
            break; // stalled — no new chunk for 5s
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let elapsed = started.elapsed();
    receiver.abort();

    let (got, bytes, lag) = {
        let tally = tally_rx.borrow();
        (tally.delivered, tally.delivered_bytes, tally.lagged)
    };
    let got =
        f64::from(u32::try_from(got).expect("delivered count never exceeds the u32 chunk count"));
    let ratio = got / f64::from(chunk_count);
    let mb = exact_f64(bytes) / (1024.0 * 1024.0);
    let secs = elapsed.as_secs_f64();

    let _ = node_a.leave().await;
    let _ = node_b.leave().await;

    println!("\n── transfer report ─────────────────────────────");
    println!("  sent       : {chunk_count} chunks ({CHUNK_BODY_LEN} B each)");
    println!(
        "  delivered  : {got}/{chunk_count}  ({:.2}%)",
        ratio * 100.0
    );
    println!("  verified   : every delivered chunk byte-exact");
    println!("  elapsed    : {secs:.2} s");
    // Each send awaits its own round-trip to the event loop, so this is
    // serialized send throughput (latency-bound), not saturated bandwidth.
    println!(
        "  send rate  : {:.1} MB/s   ({:.0} msgs/s)  [serialized]",
        mb / secs,
        got / secs
    );
    if lag > 0 {
        println!("  broadcast-lagged (dropped before verify): {lag}");
    }
    println!("────────────────────────────────────────────────");

    if ratio < 0.99 {
        eprintln!(
            "WARNING: delivery ratio {:.2}% < 99% — investigate gossip loss / broadcast lag",
            ratio * 100.0
        );
    }
}
