//! End-to-end subprocess test for `ahsw sh listen` / `connect`: a producer
//! shares a shell (a scripted command via the hidden `--command` knob) and a
//! viewer redeems the ticket and receives the shell's output over a real, direct
//! QUIC link. Driven through the shipped binary — a wire-contract proof that the
//! ticket round-trips and the framed stream is delivered.
//!
//! The `--command`/`--rows`/`--cols` knobs are hidden test/ops flags: they make
//! the producer deterministic without a real tty (the CI runner has none).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

mod common;
use common::{CONNECT_TIMEOUT, Node, POLL, test_cmd};

/// A spawned `ahsw sh` child, killed when the test ends (or panics).
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn a `Command` with stdout piped, streaming each line onto a channel via a
/// reader thread (so the child's stdout buffer never blocks the caller).
fn spawn_piped(mut cmd: Command) -> (ChildGuard, Receiver<String>) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn ahsw sh process");
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

#[test]
fn viewer_receives_the_shared_shell_output() {
    // A loopback swarm — its id carries loopback lookups (no mDNS/DHT/relay), so
    // the session stays on this host. `_creator` is held so its Drop kills the
    // daemon at test end.
    let (_creator, swarm) = Node::create_named("sh-view");

    // Producer: share a scripted shell that prints a marker line, then lingers so
    // the viewer has time to attach (the marker is kept in the replay buffer, so
    // a viewer joining just after the print still receives it). `--output json`
    // prints exactly one line — the `ahsw sh connect <ticket>` command.
    let mut producer_cmd = test_cmd();
    producer_cmd.args([
        "sh",
        "listen",
        "--swarm",
        swarm.as_str(),
        "--command",
        "printf 'AHSW-SH-OK\\n'; sleep 10",
        "--cols",
        "80",
        "--rows",
        "24",
        "--output",
        "json",
    ]);
    let (_producer, producer_rx) = spawn_piped(producer_cmd);
    let ticket_line = recv_line_containing(&producer_rx, "sh connect")
        .expect("producer never printed an sh connect command");
    let ticket = ticket_line
        .split_whitespace()
        .nth(3)
        .expect("sh connect line missing ticket token")
        .to_string();

    // Viewer: redeem the ticket. Its stdout is piped (not a tty), so it renders
    // raw — the marker line lands verbatim on stdout.
    let mut viewer_cmd = test_cmd();
    viewer_cmd.args(["sh", "connect", ticket.as_str()]);
    let (_viewer, viewer_rx) = spawn_piped(viewer_cmd);

    assert!(
        recv_line_containing(&viewer_rx, "AHSW-SH-OK").is_some(),
        "viewer never received the shared shell's output"
    );
}

#[test]
fn write_viewer_input_reaches_the_shell() {
    let (_creator, swarm) = Node::create_named("sh-write");

    // Producer: `--write` mints a second ticket; in json mode the read connect
    // command prints first and the write command second. The scripted shell
    // consumes one line and echoes it back — proof the input traversed the PTY —
    // then lingers so delivery to the viewer completes.
    let mut producer_cmd = test_cmd();
    producer_cmd.args([
        "sh",
        "listen",
        "--swarm",
        swarm.as_str(),
        "--write",
        "--command",
        "read line; echo \"GOT-$line\"; sleep 10",
        "--cols",
        "80",
        "--rows",
        "24",
        "--output",
        "json",
    ]);
    let (_producer, producer_rx) = spawn_piped(producer_cmd);
    let _read_line = recv_line_containing(&producer_rx, "sh connect")
        .expect("producer never printed the read connect command");
    let write_line = recv_line_containing(&producer_rx, "sh connect")
        .expect("producer never printed the write connect command");
    let write_ticket = write_line
        .split_whitespace()
        .nth(3)
        .expect("write connect line missing ticket token")
        .to_string();

    // Viewer: redeem the write ticket with piped stdin — the non-tty path
    // forwards stdin verbatim as input frames (no escape scanning).
    let mut viewer_cmd = test_cmd();
    viewer_cmd.args(["sh", "connect", write_ticket.as_str()]);
    viewer_cmd.stdin(Stdio::piped());
    let (mut viewer, viewer_rx) = spawn_piped(viewer_cmd);
    let mut viewer_stdin = viewer.0.stdin.take().expect("viewer stdin handle");
    viewer_stdin
        .write_all(b"hello\n")
        .expect("writing to the viewer's stdin failed");
    // EOF FINs the viewer's send half; the producer treats that as benign and
    // the viewer keeps watching.
    drop(viewer_stdin);

    assert!(
        recv_line_containing(&viewer_rx, "GOT-hello").is_some(),
        "the shell never echoed the viewer's input back"
    );
}
