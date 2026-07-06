//! Circuit multi-hop end-to-end: pipe a known payload from one end node to the
//! far end node over a **multi-hop circuit** relayed hop-by-hop through two
//! intermediate nodes.
//!
//! Four subprocesses form a real loopback mesh capped at `--max-peers 2`, so the
//! route from the sender to a non-neighbour receiver is genuinely multi-hop.
//! With `--no-unicast` and `--no-gossip-directed`, the circuit is the **only**
//! directed transport — the engine's directed-send tier order is
//! unicast → circuit → gossip, and both the unicast tier and the gossip-directed
//! fallback are off. So if the receiver's stdout shows the bytes, they can only
//! have arrived over a circuit relayed across the partial mesh (mirrors the a2a
//! suite's multi-hop directed-delivery coverage).
//!
//! The relays are not the addressee, so their `on_app_frame` never fires for the
//! directed frame — they forward at the engine's circuit layer only. Only the
//! receiver writes bytes to stdout.
//!
//! # Plaintext directed delivery to a non-a2a addressee
//!
//! Sealing is **app-controlled** per frame (`AppClass.sealed`): the engine's
//! inbound gate only unseals a directed frame addressed to us when the app's
//! `classify` marks the tag sealed. a2a's directed tags stay sealed
//! (unseal-or-drop, no security change); mesh-pipe marks its
//! `pipe_data`/`pipe_eof` tags `sealed: false`, so the addressee passes the
//! plaintext (base64, unsealed) body straight through to `on_app_frame`.
//!
//! The frame is still signature-authenticated before the gate — `sealed: false`
//! only means "not additionally encrypted", mesh-pipe's explicit choice (it
//! publishes no a2a card, so it can neither seal to a peer nor be sealed to). So
//! circuit *transport* carries the opaque directed frame across the partial mesh
//! and the receiver's engine hands the plaintext body to the app: directed
//! (`--to`) delivery over a multi-hop circuit works end-to-end.

use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

/// The circuit is the sole directed transport, and the active view is capped so
/// the mesh is genuinely partial (routes to non-neighbours are multi-hop).
const CIRCUIT_ONLY: [&str; 4] = ["--max-peers", "2", "--no-unicast", "--no-gossip-directed"];

/// Pull the minted `🐝…` id out of the sender's stderr line
/// (`mesh-pipe: mesh 🐝…`).
async fn read_mesh_id(reader: &mut BufReader<tokio::process::ChildStderr>) -> Option<String> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await.ok()?;
        if read == 0 {
            return None;
        }
        if let Some(rest) = line.trim().strip_prefix("mesh-pipe: mesh ") {
            return Some(rest.trim().to_owned());
        }
    }
}

async fn reap(mut child: Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Spawn a `connect` joiner (a relay or the receiver) under the circuit-only
/// policy. `pipe_stdout` is true only for the receiver, whose stdout the test
/// reads; the relays inherit stderr and drop stdout.
fn spawn_joiner(bin: &str, mesh_id: &str, nick: &str, pipe_stdout: bool) -> Child {
    let stdout = if pipe_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    Command::new(bin)
        .arg("connect")
        .arg(mesh_id)
        .arg("--nick")
        .arg(nick)
        .args(CIRCUIT_ONLY)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn connect joiner")
}

#[tokio::test]
async fn payload_relays_across_a_circuit() {
    let bin = env!("CARGO_BIN_EXE_mesh-pipe");
    let payload = "hello from the far end of a circuit — 🐝 hop by hop\n";

    // Sender: create the loopback mesh, print its id, read stdin. Directed
    // `--to receiver` under circuit-only policy.
    let mut sender = Command::new(bin)
        .arg("listen")
        .arg("--to")
        .arg("receiver")
        .arg("--nick")
        .arg("sender")
        .args(CIRCUIT_ONLY)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn listen");

    let mut sender_stderr = BufReader::new(sender.stderr.take().expect("sender stderr"));
    let mesh_id = tokio::time::timeout(Duration::from_secs(15), read_mesh_id(&mut sender_stderr))
        .await
        .expect("timed out waiting for the mesh id")
        .expect("sender printed a mesh id");

    // Drain the sender's remaining stderr so its pipe never fills and blocks it.
    tokio::spawn(async move {
        let mut sink = String::new();
        loop {
            sink.clear();
            if sender_stderr.read_line(&mut sink).await.unwrap_or(0) == 0 {
                break;
            }
        }
    });

    // Two relays and the receiver join the same mesh. Only the receiver's
    // stdout is piped (the test reads it); the relays just forward circuits.
    let relay_a = spawn_joiner(bin, &mesh_id, "relay-a", false);
    let relay_b = spawn_joiner(bin, &mesh_id, "relay-b", false);
    let mut receiver = spawn_joiner(bin, &mesh_id, "receiver", true);
    let mut receiver_stdout = receiver.stdout.take().expect("receiver stdout");

    // Let the four peers mesh, exchange signed `PeerInfo` (so the sender learns
    // the receiver's endpoint — the binding a directed frame is routed on), and
    // let link-state gossip converge so a circuit path to the (non-neighbour)
    // receiver exists before we rely on it.
    tokio::time::sleep(Duration::from_secs(15)).await;

    // A single directed frame is fire-and-forget: `send_app` routes it once, and
    // under circuit-only policy a frame sent before the sender knows the
    // receiver's endpoint (or before a circuit path exists) is simply dropped —
    // there is no unicast tier and no gossip-directed fallback. Routes converge
    // asynchronously, so retry: keep re-feeding the payload on the sender's stdin
    // (each write becomes one more directed `pipe_data` frame) until the
    // receiver's stdout shows it, or the deadline passes. stdin is held open the
    // whole time, so the sender never emits `pipe_eof`; we detect success by the
    // payload appearing, not by EOF.
    let mut sender_stdin = sender.stdin.take().expect("sender stdin");
    let deadline = Instant::now() + Duration::from_secs(100);
    let payload_bytes = payload.as_bytes();
    let contains_payload = |haystack: &[u8]| {
        haystack
            .windows(payload_bytes.len())
            .any(|window| window == payload_bytes)
    };

    let mut collected = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut last_feed: Option<Instant> = None;
    while Instant::now() < deadline && !contains_payload(&collected) {
        // Re-feed every few seconds (and immediately on the first pass) until the
        // payload lands — each write is one more directed-send attempt across the
        // converging mesh.
        let due = last_feed.is_none_or(|at| at.elapsed() >= Duration::from_secs(4));
        if due {
            if sender_stdin.write_all(payload_bytes).await.is_err()
                || sender_stdin.flush().await.is_err()
            {
                break; // sender gone
            }
            last_feed = Some(Instant::now());
        }
        // Read whatever has arrived, bounded so we loop back to re-feed.
        match tokio::time::timeout(Duration::from_secs(2), receiver_stdout.read(&mut chunk)).await {
            Ok(Ok(read)) if read > 0 => collected.extend_from_slice(&chunk[..read]),
            Ok(Ok(_) | Err(_)) => break, // receiver closed stdout / io error
            Err(_) => {}                 // read timed out: loop back and re-feed
        }
    }

    drop(sender_stdin);
    reap(sender).await;
    reap(relay_a).await;
    reap(relay_b).await;
    reap(receiver).await;

    assert!(
        contains_payload(&collected),
        "the payload must reach the far receiver over a circuit relayed \
         through the intermediates (unicast + gossip-directed are off, so no \
         other directed path exists); received {} bytes: {:?}",
        collected.len(),
        String::from_utf8_lossy(&collected),
    );
}
