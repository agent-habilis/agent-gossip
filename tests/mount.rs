//! End-to-end subprocess test for `ahsw mount serve` / `ahsw mount`: a real
//! tree is served over a loopback swarm through the shipped binary, and the
//! consumer redeems the ticket with `--no-mount` — proving the CLI wiring,
//! ticket round-trip, manifest fetch, tree build, and the loopback NFS bridge
//! bind all work through the actual process, without touching the OS mount
//! table (that needs privileges CI does not have).
//!
//! The byte-exact protocol details (framing, ranged reads, bad secret) are
//! covered in-process in `src/mount/`; `real_mount_round_trip` below does the
//! actual OS mount and is `#[ignore]`d — run it by hand.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

mod common;
use common::{CONNECT_TIMEOUT, Node, POLL, test_cmd};

/// A spawned `ahsw mount` child killed when the test ends (or panics).
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
            "ahsw-mount-it-{}-{tag}-{unique}",
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
        .expect("failed to spawn ahsw mount process");
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

/// Serve `root` over a loopback swarm and return the producer guard plus the
/// `🐝…` mount ticket parsed from its stdout.
fn spawn_producer(root: &Path, swarm: &str) -> (ChildGuard, String) {
    let mut producer_cmd = test_cmd();
    producer_cmd.args([
        "mount",
        "serve",
        root.to_str().expect("utf-8 path"),
        "--swarm",
        swarm,
        "--output",
        "json",
    ]);
    let (producer, producer_rx) = spawn_piped(producer_cmd);
    let ticket_line = recv_line_containing(&producer_rx, "ahsw mount")
        .expect("producer never printed a mount command");
    let ticket = ticket_line
        .split_whitespace()
        .nth(2)
        .expect("mount line missing ticket token")
        .to_string();
    (producer, ticket)
}

#[test]
fn serves_a_ticket_and_the_bridge_binds() {
    // A loopback swarm — its id carries loopback lookups (no mDNS/DHT/relay),
    // so everything stays on this host. `_creator` is held so its Drop kills
    // the daemon at test end.
    let (_creator, swarm) = Node::create_named("mount-bridge");

    let src = TempDir::new("src");
    let root = src.path.join("dataset");
    write_file(&root.join("readme.md"), b"# mounted");
    write_file(&root.join("data/blob.bin"), &vec![7u8; 10_000]);

    let (_producer, ticket) = spawn_producer(&root, swarm.as_str());
    assert!(ticket.starts_with("🐝"), "ticket token, got: {ticket}");

    // Consumer with --no-mount: redeems the ticket, fetches the manifest,
    // builds the tree, binds the NFS bridge, and prints the OS mount command
    // (json mode: the bare command) — everything except the privileged step.
    let mountpoint = TempDir::new("mnt");
    let mut consumer_cmd = test_cmd();
    consumer_cmd.args([
        "mount",
        &ticket,
        mountpoint.path.to_str().expect("utf-8 mountpoint"),
        "--no-mount",
        "--output",
        "json",
    ]);
    let (_consumer, consumer_rx) = spawn_piped(consumer_cmd);
    let command_line = recv_line_containing(&consumer_rx, "port=")
        .expect("consumer never printed the OS mount command");
    assert!(
        command_line.contains("127.0.0.1:/"),
        "expected an NFS mount command, got: {command_line}"
    );
    assert!(
        command_line.contains(mountpoint.path.to_str().unwrap()),
        "mount command names the mountpoint, got: {command_line}"
    );
}

/// The full thing: serve, actually mount through the OS NFS client, read a
/// file byte-for-byte, verify writes fail, unmount. Needs a real mount
/// permission, so it never runs in CI — run it by hand:
/// `cargo test --test mount -- --ignored`
#[test]
#[ignore = "performs a real OS mount; run manually"]
fn real_mount_round_trip() {
    let (_creator, swarm) = Node::create_named("mount-real");

    let src = TempDir::new("src");
    let root = src.path.join("dataset");
    write_file(&root.join("hello.txt"), b"hello from the other side");

    let (_producer, ticket) = spawn_producer(&root, swarm.as_str());

    let mountpoint = TempDir::new("mnt");
    let mut consumer_cmd = test_cmd();
    consumer_cmd.args([
        "mount",
        &ticket,
        mountpoint.path.to_str().expect("utf-8 mountpoint"),
    ]);
    let (_consumer, consumer_rx) = spawn_piped(consumer_cmd);
    recv_line_containing(&consumer_rx, "Mounted").expect("consumer never reported Mounted");

    let mounted_file = mountpoint.path.join("hello.txt");
    assert_eq!(
        std::fs::read(&mounted_file).expect("read through the mount"),
        b"hello from the other side"
    );
    assert!(
        std::fs::write(mountpoint.path.join("new.txt"), b"nope").is_err(),
        "the mount must be read-only"
    );

    // Unmount before the guards kill the processes, so the tempdir can drop.
    let _ = Command::new("umount")
        .arg(&mountpoint.path)
        .output()
        .expect("run umount");
}
