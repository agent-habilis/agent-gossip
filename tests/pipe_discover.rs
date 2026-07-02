//! Live ticket advertise → discover → connect, end to end, over the
//! loopback ladder: `pipe listen --advertise` re-broadcasts its bearer
//! ticket into a private directory, `pipe discover --output json` surfaces
//! it as `ticket_found`, and the surfaced ticket is redeemable — the full
//! receiver path without ever copying a `🐝…` by hand. Kills the producer
//! at the end to prove the listing ages out (`ticket_lost`). Mirrors
//! `tests/directory.rs` (the swarm-ad counterpart) via the same hidden
//! `--directory-private` + short-timing flags.

mod common;

use std::fs::{self, File};
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use common::{CONNECT_TIMEOUT, Node, POLL, test_cmd, tmp_log};

/// Loopback directory + fast timings so the test runs in seconds.
const DIR_FLAGS: [(&str, &str); 5] = [
    ("--directory-private", ""),
    ("--beacon-cohost-grace-secs", "2"),
    ("--advertise-interval-secs", "2"),
    ("--directory-expiry-secs", "4"),
    ("--alive-timeout-secs", "5"),
];

/// First log line containing `needle`, or `None` if `timeout` elapses.
fn wait_for_line(log: &Path, needle: &str, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let content = fs::read_to_string(log).unwrap_or_default();
        if let Some(line) = content.lines().find(|line| line.contains(needle)) {
            return Some(line.to_string());
        }
        std::thread::sleep(POLL);
    }
    None
}

fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn pipe_advertise_then_discover_then_connect() {
    // A loopback swarm id: its embedded lookups keep the pipe *and* the
    // directory it advertises into on this host (matching the discoverer's
    // `--directory-private` loopback resolution). `_creator` is held so its
    // Drop kills the daemon at test end.
    let (_creator, swarm) = Node::create_named("pipe-dir");

    // The payload the pipe serves — a real file, so the producer takes the
    // seekable fan-out path (required by `--advertise`: it must stay up).
    let payload = tmp_log("pipe-dir-payload");
    fs::write(&payload, b"discovered-pipe-payload").expect("write payload");

    // Producer: serve the file and advertise the ticket in `ptest`.
    let prod_log = tmp_log("pipe-dir-prod");
    let prod_file = File::create(&prod_log).unwrap();
    let mut producer = test_cmd()
        .args([
            "pipe",
            "listen",
            "--swarm",
            swarm.as_str(),
            "--advertise",
            "ptest",
            "--output",
            "json",
        ])
        .args(common::flag_args(&DIR_FLAGS))
        .stdin(Stdio::from(File::open(&payload).expect("open payload")))
        .stdout(Stdio::from(prod_file.try_clone().unwrap()))
        .stderr(Stdio::from(prod_file))
        .spawn()
        .expect("spawn producer");

    // The producer's json stdout is the bare connect command — grab the
    // ticket it advertises so the discovered one can be matched against it.
    let Some(connect_line) = wait_for_line(&prod_log, "pipe connect", CONNECT_TIMEOUT) else {
        reap(&mut producer);
        panic!(
            "producer never printed its connect command\nlog:\n{}",
            fs::read_to_string(&prod_log).unwrap_or_default()
        );
    };
    let advertised_ticket = connect_line
        .split_whitespace()
        .nth(3)
        .expect("connect line carries the ticket")
        .to_string();

    // Discoverer (agent mode): browse `ptest` and stream ticket events.
    let disc_log = tmp_log("pipe-dir-disc");
    let disc_file = File::create(&disc_log).unwrap();
    let mut discoverer = test_cmd()
        .args(["pipe", "discover", "ptest", "--output", "json"])
        .args(common::flag_args(&DIR_FLAGS))
        .stdout(Stdio::from(disc_file.try_clone().unwrap()))
        .stderr(Stdio::from(disc_file))
        .spawn()
        .expect("spawn discoverer");

    // The advertised ticket should surface as `ticket_found`.
    let found = wait_for_line(&disc_log, "\"event\":\"ticket_found\"", CONNECT_TIMEOUT);
    let found_ok = found
        .as_deref()
        .is_some_and(|line| line.contains(&advertised_ticket));
    if !found_ok {
        let prod = fs::read_to_string(&prod_log).unwrap_or_default();
        let disc = fs::read_to_string(&disc_log).unwrap_or_default();
        reap(&mut producer);
        reap(&mut discoverer);
        panic!("discoverer never surfaced the advertised ticket\nprod:\n{prod}\ndisc:\n{disc}");
    }
    // The ad carries the producer's label — the served file's name.
    let found_line = found.expect("checked above");
    assert!(
        found_line.contains("pipe-dir-payload"),
        "the ad should label the ticket with the source file name: {found_line}"
    );

    // The surfaced ticket is redeemable: connect and pull the payload.
    let fetched = test_cmd()
        .args(["pipe", "connect", advertised_ticket.as_str()])
        .output()
        .expect("run pipe connect");
    assert!(
        fetched.status.success(),
        "pipe connect on the discovered ticket failed: {}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    assert_eq!(
        fetched.stdout, b"discovered-pipe-payload",
        "the discovered ticket must serve the payload byte-for-byte"
    );

    // Producer exits → ads stop → the listing ages out → `ticket_lost`.
    reap(&mut producer);
    let lost = wait_for_line(
        &disc_log,
        "\"event\":\"ticket_lost\"",
        Duration::from_secs(30),
    );
    let lost_ok = lost
        .as_deref()
        .is_some_and(|line| line.contains(&advertised_ticket));

    let disc = fs::read_to_string(&disc_log).unwrap_or_default();
    reap(&mut discoverer);
    let _ = fs::remove_file(&prod_log);
    let _ = fs::remove_file(&disc_log);
    let _ = fs::remove_file(&payload);

    assert!(
        lost_ok,
        "discoverer never reported ticket_lost after the producer exited\ndisc:\n{disc}"
    );
}

/// An advertising producer must die on plain **SIGTERM** (`kill <pid>`).
/// The in-process directory session must not register process-wide signal
/// handlers — doing so suppresses the OS default-terminate and the fan-out
/// serve loop lives forever. Companion to the SIGINT regression test in
/// `tests/file_discover.rs`, covering the other signal + command.
#[test]
fn advertising_producer_stops_on_sigterm() {
    let (_creator, swarm) = Node::create_named("pipe-dir-sigterm");
    let payload = tmp_log("pipe-dir-sigterm-payload");
    fs::write(&payload, b"x").expect("write payload");

    let prod_log = tmp_log("pipe-dir-sigterm-prod");
    let prod_file = File::create(&prod_log).unwrap();
    let mut producer = test_cmd()
        .args([
            "pipe",
            "listen",
            "--swarm",
            swarm.as_str(),
            "--advertise",
            "sigtermtest",
            "--output",
            "json",
        ])
        .args(common::flag_args(&DIR_FLAGS))
        .stdin(Stdio::from(File::open(&payload).expect("open payload")))
        .stdout(Stdio::from(prod_file.try_clone().unwrap()))
        .stderr(Stdio::from(prod_file))
        .spawn()
        .expect("spawn producer");

    if wait_for_line(&prod_log, "pipe connect", CONNECT_TIMEOUT).is_none() {
        reap(&mut producer);
        panic!(
            "producer never printed its connect command\nlog:\n{}",
            fs::read_to_string(&prod_log).unwrap_or_default()
        );
    }
    // Give the embedded directory session a beat to finish its own setup
    // (where the old bug registered the handlers).
    std::thread::sleep(Duration::from_secs(2));

    let _ = std::process::Command::new("kill")
        .arg(producer.id().to_string())
        .status();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline {
        if producer.try_wait().expect("try_wait").is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(POLL);
    }
    reap(&mut producer);
    let _ = fs::remove_file(&payload);
    let _ = fs::remove_file(&prod_log);

    assert!(
        exited,
        "an advertising `pipe listen` did not exit within 5s of SIGTERM (hang regressed)"
    );
}

/// `--advertise` on a source that can't be re-served (a non-seekable stdin
/// pipe without `--follow`) is a hard error, not a silently dead listing.
#[test]
fn advertise_rejects_a_single_shot_stdin_pipe() {
    let (_creator, swarm) = Node::create_named("pipe-dir-guard");
    let output = test_cmd()
        .args([
            "pipe",
            "listen",
            "--swarm",
            swarm.as_str(),
            "--advertise",
            "gtest",
            "--output",
            "json",
        ])
        .args(common::flag_args(&DIR_FLAGS))
        .stdin(Stdio::piped()) // a pipe: non-seekable, single-shot
        .output()
        .expect("run pipe listen");
    assert!(
        !output.status.success(),
        "advertising a single-shot stdin pipe must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("re-servable"),
        "the error should explain the re-servable requirement: {stderr}"
    );
}
