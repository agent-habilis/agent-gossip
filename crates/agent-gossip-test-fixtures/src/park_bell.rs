use std::process::{Child, Stdio};

use crate::{BELL_GRACE, assert_bell_parked, test_cmd};

/// Spawn a bell (`poll --long`) against a live daemon and prove it parked.
///
/// Unlike [`cli_poll_long`], the bell is a live child, so a test can go on to
/// assert it is *still* parked ([`assert_bell_parked`]) or that something rang
/// it ([`wait_bell_output`]). The grace period must clear the CLI's own
/// re-issue cycle, or a bell that fired immediately would still look parked.
///
/// Drain the daemon with a foreground [`cli_poll`] first: `--long` fires on any
/// waking event past `last_served`, which only a foreground poll advances, so
/// an undrained backlog (a join, a presence line) rings the bell at once.
pub fn park_bell(mesh: &str, nickname: &str) -> Child {
    let mut bell = test_cmd()
        .args(["poll", "--gossip", mesh, "--nickname", nickname, "--long"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("bell spawns");
    std::thread::sleep(BELL_GRACE);
    assert_bell_parked(&mut bell, "bell must park on a drained daemon");
    bell
}
