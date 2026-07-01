//! End-to-end subprocess test for `ahsw file send` / `get`: a real tree is
//! served over a loopback swarm through the shipped binary, received into a
//! destination directory, and then re-synced — the second `get` transfers
//! nothing because the destination already matches (the delta path).
//!
//! The byte-exact protocol details (framing, hashing, path-traversal rejection)
//! are covered in-process in `src/file/`; this is the black-box proof that the
//! CLI wiring, ticket round-trip, and delta all work through the actual process.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

mod common;
use common::{CONNECT_TIMEOUT, Node, POLL, test_cmd};

/// A spawned `ahsw file` child killed when the test ends (or panics).
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A throwaway directory under the OS temp dir, removed recursively on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ahsw-file-it-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_file(path: &Path, contents: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("path has a parent")).expect("create parent");
    std::fs::write(path, contents).expect("write file");
}

/// Spawn a `Command` with stdout piped, streaming each line onto a channel via a
/// reader thread (so the caller never blocks the child's stdout buffer).
fn spawn_piped(mut cmd: Command) -> (ChildGuard, Receiver<String>) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn ahsw file process");
    let stdout = child.stdout.take().expect("child stdout handle");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(text) => {
                    if tx.send(text).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    (ChildGuard(child), rx)
}

/// Wait (up to `CONNECT_TIMEOUT`) for a stdout line containing `needle`.
fn recv_line_containing(rx: &Receiver<String>, needle: &str) -> Option<String> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        match rx.recv_timeout(POLL) {
            Ok(line) if line.contains(needle) => return Some(line),
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

/// Run `ahsw file get <ticket> --out <dest>` to completion and return its
/// (human-mode) stdout — the `Received …` summary line.
fn run_consumer(ticket: &str, dest: &Path) -> String {
    let output = test_cmd()
        .args([
            "file",
            "get",
            ticket,
            "--out",
            dest.to_str().expect("utf-8 dest"),
        ])
        .output()
        .expect("run file get");
    assert!(
        output.status.success(),
        "file get failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn transfers_a_tree_then_resyncs_only_the_delta() {
    // A loopback swarm — its id carries loopback lookups (no mDNS/DHT/relay), so
    // the transfer stays on this host. `_creator` is held so its Drop kills the
    // daemon at test end.
    let (_creator, swarm) = Node::create_named("file-xfer");

    // Build a small nested source tree.
    let src = TempDir::new("src");
    let root = src.path.join("project");
    write_file(&root.join("readme.md"), b"# hello");
    write_file(&root.join("src/main.rs"), b"fn main() {}");
    write_file(&root.join("data/blob.bin"), &vec![9u8; 50_000]);

    // Sender: serve the tree. `--output json` prints exactly one line — the
    // ready-to-run `ahsw file get <ticket>` command.
    let mut producer_cmd = test_cmd();
    producer_cmd.args([
        "file",
        "send",
        root.to_str().expect("utf-8 path"),
        "--swarm",
        swarm.as_str(),
        "--output",
        "json",
    ]);
    let (_producer, producer_rx) = spawn_piped(producer_cmd);
    let ticket_line = recv_line_containing(&producer_rx, "file get")
        .expect("sender never printed a file get command");
    let ticket = ticket_line
        .split_whitespace()
        .nth(3)
        .expect("file get line missing ticket token")
        .to_string();

    // First receive into an empty destination — the whole tree transfers.
    let dst = TempDir::new("dst");
    let summary1 = run_consumer(&ticket, &dst.path);
    let landed = dst.path.join("project");
    assert_eq!(std::fs::read(landed.join("readme.md")).unwrap(), b"# hello");
    assert_eq!(
        std::fs::read(landed.join("src/main.rs")).unwrap(),
        b"fn main() {}"
    );
    assert_eq!(
        std::fs::read(landed.join("data/blob.bin")).unwrap(),
        vec![9u8; 50_000]
    );
    assert!(
        summary1.contains("Received") && summary1.contains("3 files"),
        "first run should send all 3 files, got: {summary1}"
    );

    // Second receive into the now-populated destination — nothing changed, so
    // the delta transfers zero files.
    let summary2 = run_consumer(&ticket, &dst.path);
    assert!(
        summary2.contains("0 files") && summary2.contains("3 unchanged"),
        "second run should be a zero-diff re-sync, got: {summary2}"
    );
}
