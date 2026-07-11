//! Live advertise → discover, end to end, over the loopback ladder.
//!
//! The directory mesh is hardcoded public in normal operation, so this
//! path can't run against the public relay in CI. The `--directory-private`
//! flag flips the directory to private (loopback ladder) and relaxes the
//! `--advertise` requires-`--public` guard, so the whole pipeline —
//! advertiser → directory mesh → discoverer → `square_found`/`square_lost`
//! — runs hermetically. This is the regression guard for the directory
//! bootstrap fix (a discoverer never co-hosts; only the advertiser does).

mod common;

use std::fs::{self, File};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{CONNECT_TIMEOUT, POLL, test_cmd, tmp_log};

/// Loopback directory + fast timings so the test runs in seconds:
/// short co-host grace (advertiser becomes the directory beacon fast),
/// frequent re-ads, and a short expiry (quick `square_lost`).
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
fn directory_advertise_then_discover() {
    // Advertiser: a private mesh that lists itself in directory `dtest`.
    let adv_log = tmp_log("dir-adv");
    let adv_file = File::create(&adv_log).unwrap();
    let mut advertiser = test_cmd()
        .args(["create", "--advertise", "dtest", "--nickname", "adv"])
        .args(common::flag_args(&DIR_FLAGS))
        .stdout(Stdio::from(adv_file.try_clone().unwrap()))
        .stderr(Stdio::from(adv_file))
        .spawn()
        .expect("spawn advertiser");

    // Pull the advertised mesh id out of the advertiser's `ready` event.
    let ready = wait_for_line(&adv_log, "\"event\":\"ready\"", CONNECT_TIMEOUT);
    let Some(ready) = ready else {
        reap(&mut advertiser);
        panic!(
            "advertiser never became ready\nlog:\n{}",
            fs::read_to_string(&adv_log).unwrap_or_default()
        );
    };
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("ready json");
    let listed_id = ready["square"].as_str().expect("mesh id").to_string();

    // Discoverer: browse `dtest` and stream directory events.
    let disc_log = tmp_log("dir-disc");
    let disc_file = File::create(&disc_log).unwrap();
    let mut discoverer = test_cmd()
        .args(["discover", "--directory", "dtest"])
        .args(common::flag_args(&DIR_FLAGS))
        .stdout(Stdio::from(disc_file.try_clone().unwrap()))
        .stderr(Stdio::from(disc_file))
        .spawn()
        .expect("spawn discoverer");

    // The advertised mesh should surface as `square_found`.
    let found = wait_for_line(&disc_log, "\"event\":\"square_found\"", CONNECT_TIMEOUT);
    let found_ok = found
        .as_deref()
        .is_some_and(|line| line.contains(&listed_id));
    if !found_ok {
        let adv = fs::read_to_string(&adv_log).unwrap_or_default();
        let disc = fs::read_to_string(&disc_log).unwrap_or_default();
        reap(&mut advertiser);
        reap(&mut discoverer);
        panic!(
            "discoverer never reported square_found for {listed_id}\nadv:\n{adv}\ndisc:\n{disc}"
        );
    }

    // Advertiser exits → ads stop → the listing ages out → `square_lost`.
    reap(&mut advertiser);
    let lost = wait_for_line(
        &disc_log,
        "\"event\":\"square_lost\"",
        Duration::from_secs(30),
    );
    let lost_ok = lost
        .as_deref()
        .is_some_and(|line| line.contains(&listed_id));

    let disc = fs::read_to_string(&disc_log).unwrap_or_default();
    reap(&mut discoverer);
    let _ = fs::remove_file(&adv_log);
    let _ = fs::remove_file(&disc_log);

    assert!(
        lost_ok,
        "discoverer never reported square_lost for {listed_id}\ndisc:\n{disc}"
    );
}

/// A running `discover` must exit on a plain **SIGTERM** (`kill <pid>`),
/// not only on SIGINT. The embed directory session registers its own
/// SIGTERM handler (suppressing the OS default-terminate), so `discover`'s
/// own loop has to break on SIGTERM too — otherwise `kill` hangs it.
/// Regression for that hang.
#[test]
fn discover_stops_on_sigterm() {
    // Advertiser so the discoverer fully comes up (and thus has the embed
    // session — and its SIGTERM handler — running) before we signal it.
    let adv_log = tmp_log("term-adv");
    let adv_file = File::create(&adv_log).unwrap();
    let mut advertiser = test_cmd()
        .args([
            "create",
            "--advertise",
            // Own directory so this runs in parallel with
            // `directory_advertise_then_discover` (a shared private
            // directory derives the same loopback ports → contention).
            "stoptest",
            "--nickname",
            "adv",
        ])
        .args(common::flag_args(&DIR_FLAGS))
        .stdout(Stdio::from(adv_file.try_clone().unwrap()))
        .stderr(Stdio::from(adv_file))
        .spawn()
        .expect("spawn advertiser");
    if wait_for_line(&adv_log, "\"event\":\"ready\"", CONNECT_TIMEOUT).is_none() {
        reap(&mut advertiser);
        panic!("advertiser never became ready");
    }

    let disc_log = tmp_log("term-disc");
    let disc_file = File::create(&disc_log).unwrap();
    let mut discoverer = test_cmd()
        .args(["discover", "--directory", "stoptest"])
        .args(common::flag_args(&DIR_FLAGS))
        .stdout(Stdio::from(disc_file.try_clone().unwrap()))
        .stderr(Stdio::from(disc_file))
        .spawn()
        .expect("spawn discoverer");

    // Proven fully up once it surfaces the advertised mesh.
    let up = wait_for_line(&disc_log, "\"event\":\"square_found\"", CONNECT_TIMEOUT).is_some();
    reap(&mut advertiser);
    if !up {
        reap(&mut discoverer);
        panic!(
            "discoverer never surfaced a mesh\ndisc:\n{}",
            fs::read_to_string(&disc_log).unwrap_or_default()
        );
    }

    // Plain SIGTERM — the conventional stop. (std `Child` has no SIGTERM.)
    let _ = Command::new("kill")
        .arg(discoverer.id().to_string())
        .status();

    // Must exit promptly; before the fix it hung indefinitely.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline {
        if discoverer.try_wait().expect("try_wait").is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(POLL);
    }
    // Always reap (harmless if it already exited) — guarantees `wait`.
    reap(&mut discoverer);
    let _ = fs::remove_file(&adv_log);
    let _ = fs::remove_file(&disc_log);

    assert!(
        exited,
        "discover did not exit within 5s of SIGTERM (hang regressed)"
    );
}
