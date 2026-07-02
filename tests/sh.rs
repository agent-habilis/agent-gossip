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
fn session_env_var_and_state_file_track_the_viewer_count() {
    let (_creator, swarm) = Node::create_named("sh-state");

    // The scripted shell IS the assertion vehicle: it derives the state file
    // path from $AHSW_SH exactly the way an external prompt segment would,
    // proves the pre-spawn write (viewer_count 0 before anyone attached), then
    // waits for the attach write (viewer_count 1). Its output reaches the
    // viewer via the replay buffer, so the markers land on the viewer's stdout.
    let mut producer_cmd = test_cmd();
    producer_cmd.args([
        "sh",
        "listen",
        "--swarm",
        swarm.as_str(),
        "--command",
        concat!(
            "state=/tmp/agent-habilis/swarm/sh/${AHSW_SH}.state.json; ",
            "printf 'ENV-%s\\n' \"$AHSW_SH\"; ",
            "grep -q '\"viewer_count\":0' \"$state\" && printf 'ZERO-OK\\n'; ",
            "while ! grep -q '\"viewer_count\":1' \"$state\"; do sleep 0.2; done; ",
            "printf 'COUNT-OK\\n'",
        ),
        "--cols",
        "80",
        "--rows",
        "24",
        "--output",
        "json",
    ]);
    let (mut producer, producer_rx) = spawn_piped(producer_cmd);
    let ticket_line = recv_line_containing(&producer_rx, "sh connect")
        .expect("producer never printed an sh connect command");
    let ticket = ticket_line
        .split_whitespace()
        .nth(3)
        .expect("sh connect line missing ticket token")
        .to_string();

    let mut viewer_cmd = test_cmd();
    viewer_cmd.args(["sh", "connect", ticket.as_str()]);
    let (_viewer, viewer_rx) = spawn_piped(viewer_cmd);

    let env_line = recv_line_containing(&viewer_rx, "ENV-")
        .expect("the shell never saw AHSW_SH in its environment");
    let sh_prefix = env_line
        .split("ENV-")
        .nth(1)
        .map(|rest| rest.trim_end().chars().take(16).collect::<String>())
        .expect("ENV- line missing the sh prefix");
    assert_eq!(
        sh_prefix.chars().count(),
        16,
        "AHSW_SH carries the 16-char prefix"
    );
    assert!(
        recv_line_containing(&viewer_rx, "ZERO-OK").is_some(),
        "the state file was not readable with viewer_count 0 before the viewer attached"
    );
    assert!(
        recv_line_containing(&viewer_rx, "COUNT-OK").is_some(),
        "the attach never surfaced as viewer_count 1 in the state file"
    );

    // COUNT-OK ends the scripted shell, so the producer exits cleanly — and
    // must have removed the state file on its way out.
    let status = producer.0.wait().expect("waiting on the producer failed");
    assert!(status.success(), "producer exited with {status}");
    let state_path = format!("/tmp/agent-habilis/swarm/sh/{sh_prefix}.state.json");
    assert!(
        !std::path::Path::new(&state_path).exists(),
        "state file survived the session's clean exit"
    );
}

#[test]
fn passworded_shell_admits_only_the_matching_password() {
    let (_creator, swarm) = Node::create_named("sh-pw");

    // Producer: protect the shell with a password. The ticket carries the
    // password flag but the raw secret alone no longer redeems.
    let mut producer_cmd = test_cmd();
    producer_cmd.args([
        "sh",
        "listen",
        "--swarm",
        swarm.as_str(),
        "--password=hunter2",
        "--command",
        "printf 'AHSW-SH-PW-OK\\n'; sleep 10",
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

    // A viewer with no password is refused before any terminal is rendered —
    // the decode sees the password flag and bails, so the child exits non-zero
    // without ever emitting the marker.
    let mut bare_cmd = test_cmd();
    bare_cmd.args(["sh", "connect", ticket.as_str()]);
    bare_cmd.stdout(Stdio::null());
    bare_cmd.stderr(Stdio::null());
    let status = bare_cmd
        .status()
        .expect("waiting on the passwordless viewer failed");
    assert!(
        !status.success(),
        "a passwordless viewer must be refused a passworded ticket"
    );

    // The matching password redeems and receives the shared output.
    let mut viewer_cmd = test_cmd();
    viewer_cmd.args(["sh", "connect", ticket.as_str(), "--password=hunter2"]);
    let (_viewer, viewer_rx) = spawn_piped(viewer_cmd);
    assert!(
        recv_line_containing(&viewer_rx, "AHSW-SH-PW-OK").is_some(),
        "a viewer with the right password never received the shared output"
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
