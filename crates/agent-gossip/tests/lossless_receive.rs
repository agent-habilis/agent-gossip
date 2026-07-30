//! The receive path the skills depend on: `ready` means the socket accepts, and
//! `poll` returns message bodies whole.
//!
//! Both matter because a harness that runs the daemon in the background renders
//! only a truncated prefix of each stdout line into the conversation, and writes
//! the rest to a file. So the skills discard the daemon's stdout entirely (that
//! redirect is pinned by `cli::agent::tests::long_running_gossip_commands_discard_stdout_and_stderr`)
//! and read content back through `poll`, which must never truncate.

use agent_gossip_test_fixtures as common;

use std::fs;
use std::process::{Child, Stdio};
use std::time::Instant;

use common::{
    CONNECT_TIMEOUT, MSG_TIMEOUT, POLL, cli_message_checked, cli_poll, test_cmd, tmp_log,
};

/// Long enough to exceed any plausible notification cap, and to make a partial
/// write obvious rather than subtle.
fn long_body() -> String {
    format!("SENTINEL_HEAD_{}_SENTINEL_TAIL", "x".repeat(5000))
}

struct Daemon {
    child: Child,
    log: Option<std::path::PathBuf>,
    /// Held only to keep the read end open: dropping it would SIGPIPE the
    /// daemon on its next print.
    pipe: Option<std::process::ChildStdout>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(log) = &self.log {
            let _ = fs::remove_file(log);
        }
    }
}

/// Spawn `create` with stdout+stderr to a log, and wait for the `ready` line.
/// Returns the daemon, its log, mesh, and nickname.
fn spawn_create(name: &str) -> (Daemon, String, String) {
    let log = tmp_log(&format!("lossless-{name}"));
    let file = fs::File::create(&log).unwrap();
    let child = test_cmd()
        .args(["create", "--name", name])
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file))
        .spawn()
        .expect("spawn create");
    let daemon = Daemon {
        child,
        log: Some(log),
        pipe: None,
    };

    let log_path = daemon.log.clone().expect("logged daemon");
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(ready) = read_ready(&log_path) {
            return (daemon, ready.0, ready.1);
        }
        std::thread::sleep(POLL);
    }
    panic!("daemon never printed a ready event");
}

/// Spawn `create` with stdout **piped**, and return the moment the `ready` line
/// is read — no polling interval in between.
///
/// The tightness is the point. Reading `ready` out of a log file that is polled
/// every few hundred milliseconds lets the daemon finish binding in the gap, so
/// an ordering bug between "printed ready" and "socket exists" is invisible.
fn spawn_create_piped(name: &str) -> (Daemon, String, String) {
    use std::io::{BufRead as _, BufReader};

    let mut child = test_cmd()
        .args(["create", "--name", name])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn create");
    let mut stdout = child.stdout.take().expect("piped stdout");
    // Own the child before anything below can panic, so the daemon is always
    // killed and reaped.
    let mut daemon = Daemon {
        child,
        log: None,
        pipe: None,
    };

    let (mesh, nick) = {
        let mut reader = BufReader::new(&mut stdout);
        let mut line = String::new();
        loop {
            line.clear();
            let read = reader.read_line(&mut line).expect("read daemon stdout");
            assert!(read != 0, "daemon exited before printing ready");
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
                && value["event"] == "ready"
            {
                break (
                    value["gossip"].as_str().expect("gossip").to_owned(),
                    value["nickname"].as_str().expect("nickname").to_owned(),
                );
            }
        }
    };
    daemon.pipe = Some(stdout);
    (daemon, mesh, nick)
}

/// `(gossip, nickname)` from the `ready` line, if it has been written.
fn read_ready(log: &std::path::Path) -> Option<(String, String)> {
    let content = fs::read_to_string(log).ok()?;
    content.lines().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value["event"] != "ready" {
            return None;
        }
        Some((
            value["gossip"].as_str()?.to_owned(),
            value["nickname"].as_str()?.to_owned(),
        ))
    })
}

/// A `ready` event means the IPC socket accepts. It used to be emitted in setup,
/// *before* the listener bound, so a client acting on it raced the socket; the
/// event now fires beside the state-file flag, at the bind.
///
/// The socket check is the load-bearing one, and it only bites when `ready` is
/// read straight off a pipe: via a polled log file the daemon finishes binding
/// in the gap and the old ordering passes too.
#[test]
fn ready_event_implies_the_ipc_socket_accepts() {
    let (_daemon, mesh, nick) = spawn_create_piped("readygate");

    let socket = common::socket_path(&mesh, &nick);
    assert!(
        std::path::Path::new(&socket).exists(),
        "`ready` was printed before the IPC socket existed — a client acting on \
         it would race the listener. missing: {socket}"
    );

    let out = test_cmd()
        .args(["peers", "--gossip", &mesh, "--nickname", &nick])
        .output()
        .expect("peers spawns");
    assert!(
        out.status.success(),
        "peers must succeed immediately after `ready`. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The 1-message create flow's load-bearing invariant: your own meta report is
/// pollable (delivered, `is_visible: false`) but non-waking — a parked
/// `poll --long` bell must NOT fire for it, and must still fire for the next
/// real message, carrying the meta echo along in the same batch.
#[test]
fn bell_ignores_own_meta_report_but_delivers_it_with_the_next_message() {
    use std::io::Read as _;

    let (_daemon, mesh, nick) = spawn_create("bellmeta");

    // The create flow's self report.
    let merge = format!(r#"{{"peers":{{"{nick}":{{"model":"test-model","status":"idle"}}}}}}"#);
    let out = test_cmd()
        .args([
            "meta",
            "merge",
            "--gossip",
            &mesh,
            "--nickname",
            &nick,
            "--merge",
            &merge,
        ])
        .output()
        .expect("meta merge spawns");
    assert!(out.status.success(), "meta merge failed");

    // Arm the bell AFTER the report landed: it must park, not fire.
    let mut bell = test_cmd()
        .args(["poll", "--gossip", &mesh, "--nickname", &nick, "--long"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("bell spawns");
    // Give an immediate fire ample time to have exited.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        bell.try_wait().expect("try_wait").is_none(),
        "the bell fired on the agent's own meta report"
    );

    // A real message wakes it, and the batch carries the meta echo along.
    cli_message_checked(&mesh, &nick, "wake up");
    let deadline = Instant::now() + MSG_TIMEOUT;
    while bell.try_wait().expect("try_wait").is_none() {
        assert!(Instant::now() < deadline, "bell never woke on a message");
        std::thread::sleep(POLL);
    }
    let mut body = String::new();
    bell.stdout
        .take()
        .expect("piped")
        .read_to_string(&mut body)
        .expect("read bell output");
    assert!(
        body.contains(r#""event":"meta""#),
        "meta rode along: {body}"
    );
    assert!(
        body.contains(r#""is_visible":false"#),
        "meta is marked not printable: {body}"
    );
    assert!(
        body.contains("wake up") && body.contains(r#""is_visible":true"#),
        "the waking message is printable: {body}"
    );
}

/// Spawn a parked bell (`poll --long`) against a live daemon and prove it is
/// still parked after a grace period.
fn park_bell(mesh: &str, nick: &str) -> Child {
    let mut bell = test_cmd()
        .args(["poll", "--gossip", mesh, "--nickname", nick, "--long"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("bell spawns");
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert!(
        bell.try_wait().expect("try_wait").is_none(),
        "bell must be parked before the daemon goes away"
    );
    bell
}

/// Wait for `child` to exit within `MSG_TIMEOUT`, returning its status.
fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(Instant::now() < deadline, "process never exited");
        std::thread::sleep(POLL);
    }
}

/// Leaving a gossip never requires stopping the bell: a daemon shutting down
/// cleanly answers parked long-polls with the shutdown sentinel, and the bell
/// exits 0 with the truthful empty batch — no error to surface, no stop-order
/// dependency between the daemon task and the bell task.
#[test]
fn bell_exits_cleanly_when_the_daemon_leaves() {
    use std::io::Read as _;

    let (daemon, mesh, nick) = spawn_create("bellclean");
    let mut bell = park_bell(&mesh, &nick);

    let out = std::process::Command::new("kill")
        .args(["-TERM", &daemon.child.id().to_string()])
        .output()
        .expect("kill spawns");
    assert!(out.status.success(), "SIGTERM failed");

    let status = wait_exit(&mut bell);
    let mut body = String::new();
    bell.stdout
        .take()
        .expect("piped")
        .read_to_string(&mut body)
        .expect("read bell output");
    assert!(
        status.success(),
        "a clean daemon shutdown must end the bell with exit 0, got {status}: {body}"
    );
    assert_eq!(body.trim(), "[]", "the sentinel never reaches stdout");
}

/// The negative twin: a daemon that dies WITHOUT announcing shutdown (SIGKILL)
/// leaves the bell to exit nonzero — an error message still means an error.
#[test]
fn bell_errors_when_the_daemon_crashes() {
    let (mut daemon, mesh, nick) = spawn_create("bellcrash");
    let mut bell = park_bell(&mesh, &nick);

    daemon.child.kill().expect("SIGKILL");
    let status = wait_exit(&mut bell);
    assert!(
        !status.success(),
        "a crashed daemon must surface as a bell error"
    );
}

/// A body far past any plausible notification cap survives `poll` unaltered.
/// This is the invariant the skills rely on: whatever a notification shows, the
/// authoritative read is complete.
#[test]
fn poll_returns_a_long_body_byte_for_byte() {
    let (_daemon, mesh, nick) = spawn_create("longbody");
    let body = long_body();
    cli_message_checked(&mesh, &nick, &body);

    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let polled = cli_poll(&mesh, &nick, None);
        if let Ok(events) = serde_json::from_str::<serde_json::Value>(&polled)
            && let Some(got) = events
                .as_array()
                .and_then(|list| list.iter().find(|event| event["type"] == "broadcast"))
                .and_then(|event| event["body"].as_str())
        {
            assert_eq!(got, body, "poll truncated or altered a 5000-char body");
            return;
        }
        assert!(Instant::now() < deadline, "message never surfaced to poll");
        std::thread::sleep(POLL);
    }
}
