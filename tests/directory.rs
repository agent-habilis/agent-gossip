//! Live advertise → discover, end to end, over the loopback ladder.
//!
//! The directory swarm is hardcoded public in normal operation, so this
//! path can't run against the public relay in CI. `AHS_DIRECTORY_PRIVATE`
//! flips the directory to private (loopback ladder) and relaxes the
//! `--advertise` requires-`--public` guard, so the whole pipeline —
//! advertiser → directory mesh → discoverer → `swarm_found`/`swarm_lost`
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
/// frequent re-ads, and a short expiry (quick `swarm_lost`).
const DIR_ENV: [(&str, &str); 5] = [
    ("AHS_DIRECTORY_PRIVATE", "1"),
    ("BEACON_COHOST_GRACE_SECS", "2"),
    ("ADVERTISE_INTERVAL_SECS", "2"),
    ("DIRECTORY_EXPIRY_SECS", "4"),
    ("ALIVE_TIMEOUT_SECS", "5"),
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
    // Advertiser: a private swarm that lists itself in directory `dtest`.
    let adv_log = tmp_log("dir-adv");
    let adv_file = File::create(&adv_log).unwrap();
    let mut advertiser = test_cmd()
        .args([
            "create",
            "--advertise",
            "dtest",
            "--nickname",
            "adv",
            "--no-interactive",
            "--output",
            "json",
        ])
        .envs(DIR_ENV.iter().copied())
        .stdout(Stdio::from(adv_file.try_clone().unwrap()))
        .stderr(Stdio::from(adv_file))
        .spawn()
        .expect("spawn advertiser");

    // Pull the advertised swarm id out of the advertiser's `ready` event.
    let ready = wait_for_line(&adv_log, "\"event\":\"ready\"", CONNECT_TIMEOUT);
    let Some(ready) = ready else {
        reap(&mut advertiser);
        panic!(
            "advertiser never became ready\nlog:\n{}",
            fs::read_to_string(&adv_log).unwrap_or_default()
        );
    };
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("ready json");
    let listed_id = ready["swarm"].as_str().expect("swarm id").to_string();

    // Discoverer: browse `dtest` and stream directory events.
    let disc_log = tmp_log("dir-disc");
    let disc_file = File::create(&disc_log).unwrap();
    let mut discoverer = test_cmd()
        .args([
            "discover",
            "--directory",
            "dtest",
            "--no-interactive",
            "--output",
            "json",
        ])
        .envs(DIR_ENV.iter().copied())
        .stdout(Stdio::from(disc_file.try_clone().unwrap()))
        .stderr(Stdio::from(disc_file))
        .spawn()
        .expect("spawn discoverer");

    // The advertised swarm should surface as `swarm_found`.
    let found = wait_for_line(&disc_log, "\"event\":\"swarm_found\"", CONNECT_TIMEOUT);
    let found_ok = found
        .as_deref()
        .is_some_and(|line| line.contains(&listed_id));
    if !found_ok {
        let adv = fs::read_to_string(&adv_log).unwrap_or_default();
        let disc = fs::read_to_string(&disc_log).unwrap_or_default();
        reap(&mut advertiser);
        reap(&mut discoverer);
        panic!("discoverer never reported swarm_found for {listed_id}\nadv:\n{adv}\ndisc:\n{disc}");
    }

    // Advertiser exits → ads stop → the listing ages out → `swarm_lost`.
    reap(&mut advertiser);
    let lost = wait_for_line(
        &disc_log,
        "\"event\":\"swarm_lost\"",
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
        "discoverer never reported swarm_lost for {listed_id}\ndisc:\n{disc}"
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
            "--no-interactive",
            "--output",
            "json",
        ])
        .envs(DIR_ENV.iter().copied())
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
        .args([
            "discover",
            "--directory",
            "stoptest",
            "--no-interactive",
            "--output",
            "json",
        ])
        .envs(DIR_ENV.iter().copied())
        .stdout(Stdio::from(disc_file.try_clone().unwrap()))
        .stderr(Stdio::from(disc_file))
        .spawn()
        .expect("spawn discoverer");

    // Proven fully up once it surfaces the advertised swarm.
    let up = wait_for_line(&disc_log, "\"event\":\"swarm_found\"", CONNECT_TIMEOUT).is_some();
    reap(&mut advertiser);
    if !up {
        reap(&mut discoverer);
        panic!(
            "discoverer never surfaced a swarm\ndisc:\n{}",
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

    assert!(exited, "discover did not exit within 5s of SIGTERM (hang regressed)");
}
