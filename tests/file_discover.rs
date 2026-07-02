//! Live ticket advertise → discover → get for `file`: `file send
//! --advertise` lists its bearer ticket in a private directory, `file
//! discover --output json` surfaces it as `ticket_found`, and the surfaced
//! ticket fetches the tree — the receiver path without copying a `🐝…`.
//! The transport-level details (delta re-sync, hashing) are covered by
//! `tests/file.rs`; this proves the directory path through the real binary.

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
fn file_advertise_then_discover_then_get() {
    // A loopback swarm id keeps the transfer *and* the directory it
    // advertises into on this host, matching `--directory-private`.
    let (_creator, swarm) = Node::create_named("file-dir");

    // The tree the sender serves.
    let src = std::env::temp_dir().join(format!("ahsw-file-dir-src-{}", std::process::id()));
    let root = src.join("shared");
    fs::create_dir_all(&root).expect("create source tree");
    fs::write(root.join("hello.txt"), b"discovered-file-payload").expect("write payload");

    // Sender: serve the tree and advertise the ticket in `ftest`.
    let send_log = tmp_log("file-dir-send");
    let send_file = File::create(&send_log).unwrap();
    let mut sender = test_cmd()
        .args([
            "file",
            "send",
            root.to_str().expect("utf-8 path"),
            "--swarm",
            swarm.as_str(),
            "--advertise",
            "ftest",
            "--output",
            "json",
        ])
        .args(common::flag_args(&DIR_FLAGS))
        .stdout(Stdio::from(send_file.try_clone().unwrap()))
        .stderr(Stdio::from(send_file))
        .spawn()
        .expect("spawn sender");

    // The sender's json stdout is the bare get command — grab the ticket.
    let Some(get_line) = wait_for_line(&send_log, "file get", CONNECT_TIMEOUT) else {
        reap(&mut sender);
        panic!(
            "sender never printed its get command\nlog:\n{}",
            fs::read_to_string(&send_log).unwrap_or_default()
        );
    };
    let advertised_ticket = get_line
        .split_whitespace()
        .nth(3)
        .expect("get line carries the ticket")
        .to_string();

    // Discoverer (agent mode): browse `ftest` and stream ticket events.
    let disc_log = tmp_log("file-dir-disc");
    let disc_file = File::create(&disc_log).unwrap();
    let mut discoverer = test_cmd()
        .args(["file", "discover", "ftest", "--output", "json"])
        .args(common::flag_args(&DIR_FLAGS))
        .stdout(Stdio::from(disc_file.try_clone().unwrap()))
        .stderr(Stdio::from(disc_file))
        .spawn()
        .expect("spawn discoverer");

    let found = wait_for_line(&disc_log, "\"event\":\"ticket_found\"", CONNECT_TIMEOUT);
    let found_ok = found
        .as_deref()
        .is_some_and(|line| line.contains(&advertised_ticket) && line.contains("shared"));
    if !found_ok {
        let send = fs::read_to_string(&send_log).unwrap_or_default();
        let disc = fs::read_to_string(&disc_log).unwrap_or_default();
        reap(&mut sender);
        reap(&mut discoverer);
        panic!(
            "discoverer never surfaced the advertised file ticket (labeled `shared`)\
             \nsend:\n{send}\ndisc:\n{disc}"
        );
    }
    reap(&mut discoverer);

    // The surfaced ticket is redeemable: get the tree into a fresh dir.
    let dst = std::env::temp_dir().join(format!("ahsw-file-dir-dst-{}", std::process::id()));
    fs::create_dir_all(&dst).expect("create destination");
    let fetched = test_cmd()
        .args([
            "file",
            "get",
            advertised_ticket.as_str(),
            "--out",
            dst.to_str().expect("utf-8 dest"),
        ])
        .output()
        .expect("run file get");
    assert!(
        fetched.status.success(),
        "file get on the discovered ticket failed: {}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    assert_eq!(
        fs::read(dst.join("shared/hello.txt")).expect("fetched file"),
        b"discovered-file-payload"
    );

    reap(&mut sender);
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&dst);
    let _ = fs::remove_file(&send_log);
    let _ = fs::remove_file(&disc_log);
}

/// An advertising sender must die on plain **ctrl-c** (SIGINT). The
/// in-process directory session must not register process-wide signal
/// handlers — doing so suppresses the OS default-terminate and the serve
/// loop (which has no signal handling of its own) lives forever.
/// Regression for exactly that hang.
#[test]
fn advertising_sender_stops_on_sigint() {
    let (_creator, swarm) = Node::create_named("file-dir-sigint");
    let payload = tmp_log("file-dir-sigint-payload");
    fs::write(&payload, b"x").expect("write payload");

    let send_log = tmp_log("file-dir-sigint-send");
    let send_file = File::create(&send_log).unwrap();
    let mut sender = test_cmd()
        .args([
            "file",
            "send",
            payload.to_str().expect("utf-8 path"),
            "--swarm",
            swarm.as_str(),
            "--advertise",
            "sigintest",
            "--output",
            "json",
        ])
        .args(common::flag_args(&DIR_FLAGS))
        .stdout(Stdio::from(send_file.try_clone().unwrap()))
        .stderr(Stdio::from(send_file))
        .spawn()
        .expect("spawn sender");

    // Fully up: the ticket line is printed after the advertiser session
    // spawned; give the embedded directory session a beat to finish its
    // own setup (where the old bug registered the handlers).
    if wait_for_line(&send_log, "file get", CONNECT_TIMEOUT).is_none() {
        reap(&mut sender);
        panic!(
            "sender never printed its get command\nlog:\n{}",
            fs::read_to_string(&send_log).unwrap_or_default()
        );
    }
    std::thread::sleep(Duration::from_secs(2));

    // Plain SIGINT — what ctrl-c delivers.
    let _ = std::process::Command::new("kill")
        .args(["-INT", &sender.id().to_string()])
        .status();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline {
        if sender.try_wait().expect("try_wait").is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(POLL);
    }
    reap(&mut sender);
    let _ = fs::remove_file(&payload);
    let _ = fs::remove_file(&send_log);

    assert!(
        exited,
        "an advertising `file send` did not exit within 5s of SIGINT (ctrl-c hang regressed)"
    );
}
