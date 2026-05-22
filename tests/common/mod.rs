#![allow(
    dead_code,
    reason = "shared integration-test helpers; not every test crate exercises every one"
)]
//! Shared test infrastructure for integration tests.

use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub(crate) const TMP_DIR: &str = "/tmp/agent-habilis-swarm";

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_mins(1);
pub(crate) const MSG_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const POLL: Duration = Duration::from_millis(250);

/// Use the freshly built test binary to avoid stale release output formats.
pub(crate) fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ahs"))
}

/// Per-test-process log dir so `cargo xtask test` never writes into
/// the operator's `/tmp/agent-habilis-swarm/logs`. The binary honors
/// `AHS_LOG_DIR`.
fn test_log_dir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("ahs-test-logs-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir.to_string_lossy().into_owned()
    })
}

/// `Command` for the built binary with `AHS_LOG_DIR` redirected to a
/// per-test temp dir. Use instead of `Command::new(bin())`.
pub(crate) fn test_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.env("AHS_LOG_DIR", test_log_dir());
    cmd
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

pub(crate) fn tmp_log(tag: &str) -> PathBuf {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "ahs-test-{}-{}-{}.log",
        tag,
        std::process::id(),
        sequence
    ))
}

pub(crate) fn socket_path(swarm: &str, nickname: &str) -> String {
    let prefix: String = swarm.chars().take(16).collect();
    format!("{TMP_DIR}/{prefix}-{nickname}.sock")
}

/// A node's tracing-sink log (distinct from its captured stdout/stderr
/// in `Node::log`). Mirrors `transport::ipc::log_file_path`: same
/// `<swarm_prefix>-<nick>` stem, under the per-test `AHS_LOG_DIR`. Use
/// this to assert on `tracing` output (warn/info) the operator stream
/// never carries.
pub(crate) fn trace_log(swarm: &str, nickname: &str) -> String {
    let prefix: String = swarm.chars().take(16).collect();
    let path = format!("{}/{prefix}-{nickname}.log", test_log_dir());
    fs::read_to_string(path).unwrap_or_default()
}

/// Wait until `count_fn` returns >= `target` or `timeout` elapses.
pub(crate) fn wait_until(count_fn: impl Fn() -> usize, target: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        let current = count_fn();
        if current >= target || Instant::now() > deadline {
            return current;
        }
        std::thread::sleep(POLL);
    }
}

// ── CLI helpers ───────────────────────────────────────────────────

/// Spawn `ahs msg …` and return the raw `Output`
/// (no success assertion — callers that test failure paths inspect
/// it). `localhost` toggles `SWARM_LOCALHOST=1`.
pub(crate) fn cli_msg_raw(
    swarm: &str,
    nickname: &str,
    body: &str,
    reply: Option<&str>,
    localhost: bool,
) -> Output {
    let mut args = vec![
        "msg",
        "--swarm",
        swarm,
        "--nickname",
        nickname,
        "--text",
        body,
    ];
    if let Some(target) = reply {
        args.extend(["--reply", target]);
    }
    let mut cmd = test_cmd();
    cmd.args(&args);
    if localhost {
        cmd.env("SWARM_LOCALHOST", "1");
    }
    cmd.output().expect("msg command failed to spawn")
}

/// `cli_msg_raw` + trim stdout. No success assertion.
pub(crate) fn cli_msg_stdout(
    swarm: &str,
    nickname: &str,
    body: &str,
    reply: Option<&str>,
    localhost: bool,
) -> String {
    let out = cli_msg_raw(swarm, nickname, body, reply, localhost);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `cli_msg_raw` + assert success + trim stdout.
pub(crate) fn cli_msg_checked(
    swarm: &str,
    nickname: &str,
    body: &str,
    reply: Option<&str>,
    localhost: bool,
) -> String {
    let out = cli_msg_raw(swarm, nickname, body, reply, localhost);
    assert!(
        out.status.success(),
        "msg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Spawn `ahs poll … --output json`, assert success,
/// return trimmed stdout.
pub(crate) fn cli_poll(swarm: &str, nickname: &str, after: Option<&str>) -> String {
    let mut args = vec![
        "poll",
        "--swarm",
        swarm,
        "--nickname",
        nickname,
        "--output",
        "json",
    ];
    if let Some(id) = after {
        args.extend(["--after", id]);
    }
    let out = test_cmd()
        .args(&args)
        .output()
        .expect("poll command failed to spawn");
    assert!(
        out.status.success(),
        "poll failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ── In-process harness (embed::SwarmSession) ──────────────────────

use agent_habilis_swarm::embed::{CreateConfig, JoinConfig, SwarmSession};
use agent_habilis_swarm::{
    Message, MessageBody, MessageId, MessageKind, Nickname, OutputEvent, PresenceSubtype,
};
use tokio::sync::mpsc::UnboundedReceiver;

/// One in-process swarm node: a real [`SwarmSession`] (real iroh
/// endpoint + the real `daemon::run` loop on a background task) plus
/// its captured [`OutputEvent`] stream. Drop-in analogue of the
/// subprocess `JsonNode`/`Node`, but everything runs in the test
/// process — so coverage is recorded and teardown is deterministic.
pub(crate) struct InProcNode {
    pub session: SwarmSession,
    rx: UnboundedReceiver<OutputEvent>,
    drained: Vec<OutputEvent>,
    pub swarm: String,
    pub nickname: String,
}

impl InProcNode {
    /// Create a new private swarm. `self.swarm` holds the `ahs…` id.
    pub(crate) async fn create(name: &str) -> Self {
        Self::from_session(
            SwarmSession::create(CreateConfig::new(name))
                .await
                .expect("in-process create failed"),
        )
    }

    /// Create a new private swarm with an explicit nickname.
    pub(crate) async fn create_with_nick(name: &str, nick: &str) -> Self {
        let mut cfg = CreateConfig::new(name);
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        Self::from_session(
            SwarmSession::create(cfg)
                .await
                .expect("in-process create failed"),
        )
    }

    fn from_session(mut session: SwarmSession) -> Self {
        let rx = session.events().expect("events() receiver");
        let swarm = session.swarm_id().to_string();
        let nickname = session.nickname().to_string();
        Self {
            session,
            rx,
            drained: Vec::new(),
            swarm,
            nickname,
        }
    }

    /// Join `swarm` (an `ahs…` id) with an explicit nickname.
    pub(crate) async fn join(swarm: &str, nickname: &str) -> Self {
        let mut cfg = JoinConfig::new(swarm);
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        Self::from_session(
            SwarmSession::join(cfg)
                .await
                .expect("in-process join failed"),
        )
    }

    /// Broadcast a plain message; returns the new message id.
    pub(crate) async fn send(&self, text: &str) -> MessageId {
        self.session
            .send(MessageBody::new(text).expect("valid body"), None)
            .await
            .expect("in-process send failed")
    }

    /// Send a message addressed to `target`; returns the new id.
    pub(crate) async fn reply(&self, target: &str, text: &str) -> MessageId {
        self.session
            .send(
                MessageBody::new(text).expect("valid body"),
                Some(Nickname::new(target).expect("valid target nickname")),
            )
            .await
            .expect("in-process reply failed")
    }

    /// Clean shutdown (broadcasts `Left`).
    pub(crate) async fn leave(self) {
        let _ = self.session.leave().await;
    }

    /// Non-blocking: move every pending captured event into the buffer.
    fn pump(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.drained.push(event);
        }
    }

    /// Every captured event so far (drains pending first).
    pub(crate) fn events(&mut self) -> &[OutputEvent] {
        self.pump();
        &self.drained
    }

    /// Captured events rendered to the documented `--output json`
    /// wire format and parsed — byte-identical to what the subprocess
    /// emitted. Mirrors the old `JsonNode::json_events()`.
    pub(crate) fn json_events(&mut self) -> Vec<serde_json::Value> {
        self.pump();
        self.drained
            .iter()
            .filter_map(agent_habilis_swarm::event_json)
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect()
    }

    /// Only `{"event":"message"}` lines (both `msg` and `presence`
    /// carry `event:"message"`).
    pub(crate) fn message_events(&mut self) -> Vec<serde_json::Value> {
        self.json_events()
            .into_iter()
            .filter(|value| value["event"] == "message")
            .collect()
    }

    /// `{"event":"message","type":"msg"}` lines only (excludes
    /// presence). Mirrors the old `JsonNode::msg_events()`.
    pub(crate) fn msg_events(&mut self) -> Vec<serde_json::Value> {
        self.json_events()
            .into_iter()
            .filter(|value| value["event"] == "message" && value["type"] == "msg")
            .collect()
    }

    /// Inbound `msg`-kind messages captured so far (includes self
    /// echoes — filter on `is_self` if needed).
    pub(crate) fn messages(&mut self) -> Vec<&Message> {
        self.pump();
        self.drained
            .iter()
            .filter_map(|event| match event {
                OutputEvent::Message { msg, .. } => Some(&**msg),
                _ => None,
            })
            .collect()
    }

    /// `msg` events from *other* peers only (drops our own echoes).
    /// The subprocess `Node` parsed only peer lines, so ports that
    /// counted "deliveries" use this.
    pub(crate) fn inbound(&mut self) -> Vec<&Message> {
        self.pump();
        self.drained
            .iter()
            .filter_map(|event| match event {
                OutputEvent::Message {
                    msg,
                    is_self: false,
                } => Some(&**msg),
                _ => None,
            })
            .collect()
    }

    /// Wait until a peer (non-self) `msg` with exactly `body` is
    /// captured, or `timeout` elapses.
    pub(crate) async fn wait_body(&mut self, body: &str, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::Message { msg, is_self: false } if msg.body.as_str() == body
                )
            })
        })
        .await
    }

    /// Wait until at least `min_count` peer (non-self) messages have
    /// been captured, or `timeout` elapses.
    pub(crate) async fn wait_inbound(&mut self, min_count: usize, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events
                .iter()
                .filter(|event| matches!(event, OutputEvent::Message { is_self: false, .. }))
                .count()
                >= min_count
        })
        .await
    }

    /// Count inbound (non-self) `msg` events whose body equals `body`.
    pub(crate) fn count_body(&mut self, body: &str) -> usize {
        self.pump();
        self.drained
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    OutputEvent::Message { msg, is_self: false } if msg.body.as_str() == body
                )
            })
            .count()
    }

    /// Number of surfaced presence events of the given kind
    /// (`joined` when `joined` is true, else `left`).
    pub(crate) fn presence_count(&mut self, joined: bool) -> usize {
        self.pump();
        let want = if joined {
            PresenceSubtype::Joined
        } else {
            PresenceSubtype::Left
        };
        self.drained
            .iter()
            .filter(|event| match event {
                OutputEvent::Presence { msg } => {
                    matches!(&msg.kind, MessageKind::Presence { subtype } if *subtype == want)
                }
                _ => false,
            })
            .count()
    }

    /// Wait until a `joined`/`left` presence for `nick` is surfaced.
    pub(crate) async fn wait_presence(
        &mut self,
        nick: &str,
        joined: bool,
        timeout: Duration,
    ) -> bool {
        let want = if joined {
            PresenceSubtype::Joined
        } else {
            PresenceSubtype::Left
        };
        self.wait_for(timeout, |events| {
            events.iter().any(|event| match event {
                OutputEvent::Presence { msg } => {
                    msg.author.to_string() == nick
                        && matches!(&msg.kind, MessageKind::Presence { subtype } if *subtype == want)
                }
                _ => false,
            })
        })
        .await
    }

    /// Wait until at least `min_count` `joined`/`left` presence events
    /// (any author) have been surfaced.
    pub(crate) async fn wait_presence_count(
        &mut self,
        joined: bool,
        min_count: usize,
        timeout: Duration,
    ) -> bool {
        let want = if joined {
            PresenceSubtype::Joined
        } else {
            PresenceSubtype::Left
        };
        self.wait_for(timeout, |events| {
            events
                .iter()
                .filter(|event| match event {
                    OutputEvent::Presence { msg } => {
                        matches!(&msg.kind, MessageKind::Presence { subtype } if *subtype == want)
                    }
                    _ => false,
                })
                .count()
                >= min_count
        })
        .await
    }

    /// Poll until `pred` over the accumulated events holds, or
    /// `timeout` elapses. Returns whether the predicate was satisfied.
    pub(crate) async fn wait_for(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&[OutputEvent]) -> bool,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump();
            if pred(&self.drained) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Convenience: wait until at least `n` inbound `msg` events are
    /// seen (any author, including self echoes).
    pub(crate) async fn wait_messages(&mut self, min_count: usize, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events
                .iter()
                .filter(|event| matches!(event, OutputEvent::Message { .. }))
                .count()
                >= min_count
        })
        .await
    }
}

/// In-process analogue of the subprocess `three_peers`: a creator
/// plus two joiners (`mon-<suffix>-a` / `-b`), all meshed in this
/// process. The swarm id is `creator.swarm`.
pub(crate) async fn three_peers(suffix: &str) -> (InProcNode, InProcNode, InProcNode) {
    let creator = InProcNode::create(&format!("mon{suffix}")).await;
    let joiner_a = InProcNode::join(&creator.swarm, &format!("mon-{suffix}-a")).await;
    let joiner_b = InProcNode::join(&creator.swarm, &format!("mon-{suffix}-b")).await;
    (creator, joiner_a, joiner_b)
}

// ── Subprocess harness (real `ahs` processes) ─────
//
// For the reliability / contract tests that must exercise the shipped
// binary: real SIGKILL / SIGSTOP-SIGCONT, real stdout, real
// Unix-socket IPC, real heal/anti-entropy timing.

pub(crate) struct Node {
    child: Child,
    log: PathBuf,
    pub nickname: String,
}

impl Node {
    /// Spawn `ahs create`, wait for ahs... and the assigned nickname.
    pub(crate) fn create() -> (Self, String) {
        Self::create_named("itest")
    }

    /// Spawn `ahs create --name <name>`. Uses a fixed name by default
    /// since tests don't care what the swarm is called — only that creation
    /// and join round-trip.
    pub(crate) fn create_named(name: &str) -> (Self, String) {
        Self::create_env(name, &[])
    }

    /// Like [`create_named`](Self::create_named) but exports extra env
    /// vars to the spawned daemon (e.g. a shortened heal cadence).
    pub(crate) fn create_env(name: &str, envs: &[(&str, &str)]) -> (Self, String) {
        let log = tmp_log("create");
        let file = File::create(&log).unwrap();
        let child = test_cmd()
            .args(["create", "--name", name])
            .envs(envs.iter().copied())
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .spawn()
            .expect("failed to spawn create");

        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut swarm_id = None;
        let mut nickname = None;

        while Instant::now() < deadline && (swarm_id.is_none() || nickname.is_none()) {
            let content = fs::read_to_string(&log).unwrap_or_default();
            for line in content.lines() {
                let trimmed = line.trim();
                if swarm_id.is_none() && line.starts_with("ahs") {
                    swarm_id = Some(line.trim().to_string());
                }
                // Both lifecycle lines end with ` as <NICK>`
                // (`created #N and joined as <nick>` /
                // `joined #N as <nick>`); presence/message
                // lines never contain ` as <`. Anchor on the last
                // ` as <` so the leading `<author>` of a message
                // line can't be mistaken for the nick.
                if nickname.is_none()
                    && let Some((_, after_as)) = trimmed.rsplit_once(" as <")
                    && let Some(end) = after_as.find('>')
                {
                    nickname = Some(after_as[..end].to_string());
                }
            }
            if swarm_id.is_none() || nickname.is_none() {
                std::thread::sleep(POLL);
            }
        }

        let swarm_id = swarm_id.expect("timed out waiting for swarm identifier");
        let nickname = nickname.unwrap_or_default();
        (
            Node {
                child,
                log,
                nickname,
            },
            swarm_id,
        )
    }

    /// Spawn `ahs join <swarm> --nickname <nickname>`.
    pub(crate) fn join(swarm: &str, nickname: &str) -> Self {
        Self::join_env(swarm, nickname, &[])
    }

    /// Like [`join`](Self::join) but exports extra env vars to the
    /// spawned daemon.
    pub(crate) fn join_env(swarm: &str, nickname: &str, envs: &[(&str, &str)]) -> Self {
        let log = tmp_log(nickname);
        let file = File::create(&log).unwrap();
        let child = test_cmd()
            .args(["join", swarm, "--nickname", nickname])
            .envs(envs.iter().copied())
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .spawn()
            .expect("failed to spawn join");
        Node {
            child,
            log,
            nickname: nickname.to_string(),
        }
    }

    pub(crate) fn log_contents(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Last `count` log lines, oldest-first — for failure diagnostics.
    pub(crate) fn log_tail(&self, count: usize) -> String {
        let content = self.log_contents();
        let lines: Vec<&str> = content.lines().collect();
        lines[lines.len().saturating_sub(count)..].join("\n")
    }

    /// Block until this node's IPC socket exists.
    /// The socket is bound inside `event_loop` after `subscribe_and_join` completes,
    /// so its presence is the most reliable "node is ready" signal.
    pub(crate) fn wait_ready(&self, swarm: &str) -> bool {
        let sock = socket_path(swarm, &self.nickname);
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            if std::path::Path::new(&sock).exists() {
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

    pub(crate) fn messages(&self) -> Vec<Msg> {
        parse_messages(&self.log_contents())
    }

    /// How many received messages match `(author, body)` exactly.
    pub(crate) fn count_from(&self, author: &str, body: &str) -> usize {
        self.messages()
            .iter()
            .filter(|msg| msg.author == author && msg.body == body)
            .count()
    }

    /// Send SIGINT to the child process (triggers the ctrl-c handler).
    pub(crate) fn sigint(&self) {
        self.signal("-INT");
    }

    /// SIGKILL the child *while alive* — an ungraceful death: no
    /// graceful `Left`, the OS reaps it; peers must detect the silent
    /// vanish via the alive-timeout. (`Drop` also kills, harmlessly,
    /// if the test didn't.)
    pub(crate) fn kill(&self) {
        self.signal("-KILL");
    }

    /// SIGSTOP — suspend the process (simulates sleep / a frozen
    /// node). It stays alive and keeps its sockets/ports bound, but
    /// runs no code, so peers eventually evict it on the
    /// alive-timeout. Resume with [`cont`](Self::cont).
    pub(crate) fn stop(&self) {
        self.signal("-STOP");
    }

    /// SIGCONT — wake a [`stop`](Self::stop)ped process.
    pub(crate) fn cont(&self) {
        self.signal("-CONT");
    }

    fn signal(&self, sig: &str) {
        let _ = Command::new("kill")
            .args([sig, &self.child.id().to_string()])
            .status();
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.log);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Msg {
    pub author: String,
    pub body: String,
}

/// Split a `<author> rest...` line into (author, rest). Returns `None` for
/// lines that don't start with an angle-bracketed author.
fn split_author_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim().strip_prefix('<')?;
    let bracket_end = rest.find('>')?;
    Some((&rest[..bracket_end], rest[bracket_end + 1..].trim()))
}

/// Parse message lines in the format:
/// - `<author>: body`
/// - `<author> → <addressee>: body`
fn parse_messages(output: &str) -> Vec<Msg> {
    let mut msgs = Vec::new();
    for line in output.lines() {
        let Some((author, after)) = split_author_line(line) else {
            continue;
        };
        if after.starts_with("has joined") || after.starts_with("has left") {
            continue;
        }
        let body = if let Some(rest) = after.strip_prefix(':') {
            rest.trim()
        } else if let Some(idx) = after.find(':') {
            after[idx + 1..].trim()
        } else {
            continue;
        };
        if !body.is_empty() {
            msgs.push(Msg {
                author: author.to_string(),
                body: body.to_string(),
            });
        }
    }
    msgs
}

/// `cli_msg_raw` with localhost on and no success assertion — the
/// real-network gossip tests' default.
pub(crate) fn cli_message_raw(swarm: &str, nickname: &str, body: &str) -> Output {
    cli_msg_raw(swarm, nickname, body, None, true)
}

/// `cli_msg_stdout` with localhost on.
pub(crate) fn cli_message(swarm: &str, nickname: &str, body: &str) -> String {
    cli_msg_stdout(swarm, nickname, body, None, true)
}

/// `wait_until` with the standard message-delivery timeout.
pub(crate) fn wait_total(total_fn: impl Fn() -> usize, target: usize) -> usize {
    wait_until(total_fn, target, MSG_TIMEOUT)
}
