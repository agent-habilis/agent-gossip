//! Two-node loopback transfer soak. Streams a large **deterministic**
//! payload from one in-process peer to another over the real gossip mesh
//! (loopback only — fully offline), verifies every delivered chunk
//! byte-exact on the receiver, and prints a throughput report.
//!
//! It is a plain `fn main()` harness (`harness = false`), not divan: a
//! single long async transfer has nothing for a sampling framework to do.
//! Uses only the public `embed` API.
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
#![allow(
    clippy::cast_precision_loss,
    reason = "throughput report math; f64 precision is irrelevant for a human summary"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use agent_square::embed::{CreateConfig, JoinConfig, MeshSession};
use agent_square::{JoinTarget, MessageBody, MeshName};
use tokio::sync::broadcast::error::RecvError;

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
    CHUNK_BODY_LEN + 512 <= agent_square::MAX_MESSAGE_SIZE,
    "chunk body leaves too little room under MAX_MESSAGE_SIZE for the JSON envelope"
);

/// Deterministic printable-ASCII payload for `index` (`SplitMix64` →
/// `33..=126`). Pure function of the index, so the receiver regenerates
/// the expected bytes independently — no shared state, order-agnostic.
fn payload_for(index: u32) -> String {
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ u64::from(index).wrapping_mul(0x2545_F491_4F6C_DD1D);
    let mut out = String::with_capacity(PAYLOAD_LEN);
    for _ in 0..PAYLOAD_LEN {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
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

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let target_mb: u64 = std::env::var("BENCH_TRANSFER_MB")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(100);
    let target_bytes = target_mb * 1024 * 1024;
    let chunk_count: u32 =
        u32::try_from((target_bytes / CHUNK_BODY_LEN as u64).max(1)).expect("chunk count fits u32");

    eprintln!("transfer bench: {target_mb} MB → {chunk_count} chunks of {CHUNK_BODY_LEN} B");

    // Node A creates a loopback mesh.
    let create_cfg = CreateConfig::new(MeshName::new("bench").expect("valid name"));
    let node_a = MeshSession::create(create_cfg).await.expect("create");

    let target: JoinTarget = node_a
        .mesh_id()
        .as_str()
        .parse()
        .expect("a freshly minted 💬 id parses");
    let node_b = MeshSession::join(JoinConfig::new(target))
        .await
        .expect("join");

    // Subscribe before sending so we see everything sent after this point.
    let mut rx = node_b.messages();

    let delivered = Arc::new(AtomicUsize::new(0));
    let delivered_bytes = Arc::new(AtomicU64::new(0));
    let lagged = Arc::new(AtomicU64::new(0));

    // Receiver: verify + tally distinct chunk indices. Owns its `seen`
    // bitset; reports progress through the shared atomics.
    let receiver = {
        let delivered = Arc::clone(&delivered);
        let delivered_bytes = Arc::clone(&delivered_bytes);
        let lagged = Arc::clone(&lagged);
        tokio::spawn(async move {
            let mut seen = vec![false; chunk_count as usize];
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        if let Some(index) = verify_chunk(msg.body.as_str(), chunk_count) {
                            let slot = &mut seen[index as usize];
                            if !*slot {
                                *slot = true;
                                delivered_bytes
                                    .fetch_add(msg.body.as_str().len() as u64, Ordering::Relaxed);
                                if delivered.fetch_add(1, Ordering::Relaxed) + 1
                                    == chunk_count as usize
                                {
                                    break;
                                }
                            }
                        }
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        lagged.fetch_add(skipped, Ordering::Relaxed);
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        })
    };

    // Pre-build every chunk body *before* the clock starts: payload
    // generation + `MessageBody` validation are not transfer work and must
    // not inflate the throughput number. (The receiver still regenerates
    // independently to verify — only the sender is inside the timed window.)
    let bodies: Vec<MessageBody> = (0..chunk_count)
        .map(|index| MessageBody::new(body_for(index)).expect("valid body"))
        .collect();

    // Warm up the mesh: resend chunk 0 until the receiver sees it (deduped
    // by index), so the timed window measures steady-state transfer, not
    // rendezvous/bootstrap latency.
    let warmup_deadline = Instant::now() + Duration::from_secs(20);
    while delivered.load(Ordering::Relaxed) == 0 {
        assert!(
            Instant::now() < warmup_deadline,
            "mesh never formed: node B received no chunk within 20s"
        );
        let _ = node_a.send(bodies[0].clone()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Timed bulk send — pure send round-trips, no generation. Resending
    // index 0 (warmup) is harmless: the receiver dedups by index.
    let started = Instant::now();
    for body in bodies {
        let _ = node_a.send(body).await;
    }

    // Wait for drain: done at N, or give up after a no-progress stall /
    // overall cap (a soft delivery miss, reported as ratio < 100%).
    let overall_deadline = started + Duration::from_secs(60 + target_mb * 3);
    let mut last_seen = delivered.load(Ordering::Relaxed);
    let mut last_progress = Instant::now();
    loop {
        let now_delivered = delivered.load(Ordering::Relaxed);
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

    let got = delivered.load(Ordering::Relaxed);
    let bytes = delivered_bytes.load(Ordering::Relaxed);
    let lag = lagged.load(Ordering::Relaxed);
    let ratio = got as f64 / f64::from(chunk_count);
    let mb = bytes as f64 / (1024.0 * 1024.0);
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
        got as f64 / secs
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
