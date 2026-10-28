use std::collections::HashSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::{
    CONNECT_TIMEOUT, Msg, POLL, apply_flags, parse_messages, scan_create_log, socket_path,
    test_cmd, tmp_log,
};

/// One `agent-gossip` daemon as a child process, with its output captured in a log file.
#[derive(Debug)]
pub struct Node {
    pub(crate) child: Child,
    pub(crate) log: PathBuf,
    /// The nickname the daemon runs under.
    pub nickname: String,
}

impl Node {
    /// Spawn `agent-gossip create`, wait for the minted id and the assigned nickname.
    pub fn create() -> (Self, String) {
        Self::create_named("itest")
    }

    /// Spawn `agent-gossip create --name <name>`. Uses a fixed name by default
    /// since tests don't care what the mesh is called — only that creation
    /// and join round-trip.
    pub fn create_named(name: &str) -> (Self, String) {
        Self::create_flags(name, &[])
    }

    /// Like [`create_named`](Self::create_named) but passes extra hidden
    /// tuning flags to the spawned daemon as `(flag, value)` pairs (e.g. a
    /// shortened heal cadence). Replaces the former env overrides.
    pub fn create_flags(name: &str, flags: &[(&str, &str)]) -> (Self, String) {
        Self::create_args(name, &[], flags)
    }

    /// Like [`create_flags`](Self::create_flags) but also passes extra raw
    /// `create` CLI args (e.g. `["--lookup", "mdns,dht,relay"]`).
    pub fn create_args(name: &str, extra: &[&str], flags: &[(&str, &str)]) -> (Self, String) {
        let log = tmp_log("create");
        let file = File::create(&log).expect("create the daemon log file");
        let mut args = vec!["create", "--name", name];
        args.extend_from_slice(extra);
        let mut command = test_cmd();
        command.args(&args);
        apply_flags(&mut command, flags);
        let stdout: Stdio = Stdio::from(
            file.try_clone()
                .expect("clone the daemon log file handle for stdout"),
        );
        let child: Child = command
            .stdout(stdout)
            .stderr(Stdio::from(file))
            .spawn()
            .expect("failed to spawn create");

        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut mesh_id = None;
        let mut nickname = None;

        while Instant::now() < deadline && (mesh_id.is_none() || nickname.is_none()) {
            let content = fs::read_to_string(&log).unwrap_or_default();
            scan_create_log(&content, &mut mesh_id, &mut nickname);
            if mesh_id.is_none() || nickname.is_none() {
                std::thread::sleep(POLL);
            }
        }

        let mesh_id = mesh_id.expect("timed out waiting for mesh identifier");
        let nickname = nickname.unwrap_or_default();
        (
            Node {
                child,
                log,
                nickname,
            },
            mesh_id,
        )
    }

    /// Spawn `agent-gossip join <mesh> --nickname <nickname>`.
    pub fn join(mesh: &str, nickname: &str) -> Self {
        Self::join_flags(mesh, nickname, &[])
    }

    /// Like [`join`](Self::join) but passes extra hidden tuning flags to the
    /// spawned daemon as `(flag, value)` pairs.
    pub fn join_flags(mesh: &str, nickname: &str, flags: &[(&str, &str)]) -> Self {
        Self::join_args(mesh, nickname, &[], flags)
    }

    /// Like [`join_flags`](Self::join_flags) but also passes extra raw
    /// `join` CLI args (e.g. `["--password=pw"]`).
    pub fn join_args(mesh: &str, nickname: &str, extra: &[&str], flags: &[(&str, &str)]) -> Self {
        let log = tmp_log(nickname);
        let file = File::create(&log).expect("create the daemon log file");
        let mut command = test_cmd();
        command.args(["join", mesh, "--nickname", nickname]);
        command.args(extra);
        apply_flags(&mut command, flags);
        let stdout: Stdio = Stdio::from(
            file.try_clone()
                .expect("clone the daemon log file handle for stdout"),
        );
        let child: Child = command
            .stdout(stdout)
            .stderr(Stdio::from(file))
            .spawn()
            .expect("failed to spawn join");
        Node {
            child,
            log,
            nickname: nickname.to_string(),
        }
    }

    /// The whole log so far; empty when the file is missing or unreadable.
    pub fn log_contents(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Last `count` log lines, oldest-first — for failure diagnostics.
    pub fn log_tail(&self, count: usize) -> String {
        let content = self.log_contents();
        let lines: Vec<&str> = content.lines().collect();
        lines[lines.len().saturating_sub(count)..].join("\n")
    }

    /// Block until this node's IPC socket exists.
    /// The socket is bound inside `event_loop` after `subscribe_and_join` completes,
    /// so its presence is the most reliable "node is ready" signal.
    pub fn wait_ready(&self, mesh: &str) -> bool {
        let sock = socket_path(mesh, &self.nickname);
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            if Path::new(&sock).exists() {
                return true;
            }
            std::thread::sleep(POLL);
        }
        eprintln!(
            "wait_ready timed out for {} (looking for {})",
            self.nickname, sock
        );
        eprintln!("log tail:\n{}", self.log_tail(10));
        false
    }

    /// Every chat message in the log, in arrival order.
    pub fn messages(&self) -> Vec<Msg> {
        parse_messages(&self.log_contents())
    }

    /// How many received messages match `(author, body)` exactly.
    pub fn count_from(&self, author: &str, body: &str) -> usize {
        self.messages()
            .iter()
            .filter(|message| message.author == author && message.body == body)
            .count()
    }

    /// How many **distinct** messages from `author` have a body starting with
    /// `prefix` — the convergence metric for the anti-entropy gap-recovery
    /// tests (each sends `prefix-{i}` and waits for the full distinct set).
    pub fn count_distinct_from(&self, author: &str, prefix: &str) -> usize {
        let messages: Vec<Msg> = self.messages();
        let matching: Vec<&Msg> = messages
            .iter()
            .filter(|message| message.author == author && message.body.starts_with(prefix))
            .collect();
        let distinct: HashSet<String> = matching
            .iter()
            .map(|message| message.body.clone())
            .collect();
        distinct.len()
    }

    /// Send SIGINT to the child process (triggers the ctrl-c handler).
    pub fn sigint(&self) {
        self.signal("-INT");
    }

    /// SIGKILL the child *while alive* — an ungraceful death: no
    /// graceful `Left`, the OS reaps it; peers must detect the silent
    /// vanish via the alive-timeout. (`Drop` also kills, harmlessly,
    /// if the test didn't.)
    pub fn kill(&self) {
        self.signal("-KILL");
    }

    /// Block until the child exits **on its own** (e.g. after
    /// [`sigint`](Self::sigint)). `Drop` SIGKILLs immediately, which
    /// truncates a graceful shutdown mid-flight — the daemon never
    /// finishes its `Left` broadcast + clean QUIC close, leaving peers
    /// a zombie link that takes iroh's idle timeout to die. Reaping
    /// here guarantees the shutdown (including socket release) fully
    /// completed before the test proceeds.
    pub fn wait_exit(&mut self) {
        let _ = self.child.wait();
    }

    /// SIGSTOP — suspend the process (simulates sleep / a frozen
    /// node). It stays alive and keeps its sockets/ports bound, but
    /// runs no code, so peers eventually evict it on the
    /// alive-timeout. Resume with [`cont`](Self::cont).
    pub fn stop(&self) {
        self.signal("-STOP");
        self.await_stopped(true);
    }

    /// SIGCONT — wake a [`stop`](Self::stop)ped process.
    pub fn cont(&self) {
        self.signal("-CONT");
        self.await_stopped(false);
    }

    // A STOP or CONT that does not land leaves a live process where the test
    // expects a frozen one (or the reverse), which fails much later with a
    // timing symptom. INT and KILL stay lenient: a test may signal a child
    // that has already exited.
    fn signal(&self, signature: &str) {
        let pid = self.child.id();
        let status = Command::new("kill")
            .args([signature, &pid.to_string()])
            .status();
        if matches!(signature, "-STOP" | "-CONT") {
            assert!(
                status.as_ref().is_ok_and(std::process::ExitStatus::success),
                "kill {signature} {pid} failed: {status:?}"
            );
        }
    }

    fn await_stopped(&self, want_stopped: bool) {
        const DEADLINE: Duration = Duration::from_secs(5);
        let pid = self.child.id();
        let started = Instant::now();
        let mut last_state = String::new();
        while started.elapsed() < DEADLINE {
            let pid_arg: String = pid.to_string();
            let stat: std::io::Result<std::process::Output> = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid_arg])
                .output();
            last_state = stat
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
                .unwrap_or_default();
            if !last_state.is_empty() && last_state.starts_with('T') == want_stopped {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "pid {pid} never {} (last ps stat: {last_state:?})",
            if want_stopped { "stopped" } else { "resumed" }
        );
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.log);
    }
}
