#![allow(
    dead_code,
    reason = "shared integration-test helpers; not every test crate exercises every one"
)]
#![expect(
    clippy::wildcard_enum_match_arm,
    reason = "OutputEvent is #[non_exhaustive]; matching it from this external test crate mandates a wildcard arm, so exhaustive enumeration is impossible"
)]
//! Shared test infrastructure for integration tests.

use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

// The single source of truth for the runtime base dir lives in the shared
// crate (a dev-dependency); re-export it so test code keeps using
// `common::RUNTIME_DIR` without a divergent copy.
pub(crate) use agent_gossip::RUNTIME_DIR;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_mins(1);
/// Steady-state delivery budget: how long a meshed peer may take to surface a
/// message/presence/task leg. The suite-wide standard for every positive
/// (adaptive, break-on-success) delivery wait. A meshed in-process round trip
/// is normally sub-second; the headroom is for a loaded CI host running the
/// suite in a **debug** build, where crypto is ~10x slower than release and two
/// concurrent in-process meshes can stall a delivery well past a tighter
/// budget. `wait_for`/`wait_until` are adaptive, so a healthy run returns
/// immediately and only a genuine stall pays the ceiling.
pub(crate) const MSG_TIMEOUT: Duration = Duration::from_mins(1);
pub(crate) const POLL: Duration = Duration::from_millis(250);

/// Budget for a delivery asserted **after a disruption** (beacon death,
/// SIGSTOP freeze, rendezvous migration, creator departure). Re-meshing waits
/// on the fixed 15s heal cadence, so a recovery that just missed a tick needs
/// another cycle — the steady-state `MSG_TIMEOUT` (1 min) sits right on that
/// cliff and flakes on a loaded host. 2 min clears ~8 heal cycles; `wait_until`
/// is adaptive, so a healthy run returns in seconds and only a genuine stall
/// pays the ceiling. The extra headroom over a bare "few cycles" is for a host
/// running real swarm daemons alongside the suite (dogfooding) — CPU starvation
/// there slows convergence past a tighter bound without any product fault. One
/// named constant so every post-disruption assertion across the suite uses the
/// same floor.
pub(crate) const RECOVERY_TIMEOUT: Duration = Duration::from_mins(2);

/// Serializes the daemon-spawning reliability tests (`#[test]`, sync) —
/// beacon migration, sleep/wake heal, anti-entropy, flap storms. They assert
/// recovery within heal-cadence-gated budgets; run concurrently (libtest
/// `--test-threads`), their active windows starve each other's timing and
/// miss those budgets — a flaky failure that is **not** a product bug (real
/// daemons mesh in ~3s; verified out-of-band). Holding this gate for the
/// test's duration lets at most one run at a time while lighter tests still
/// parallelize. Each integration binary is its own process, so this `static`
/// serializes within a binary, not across them. (The async in-process
/// adversarial suite does **not** use this — a multi-thread runtime keeps it
/// responsive; a cross-runtime async mutex starved it instead.)
static SERIAL_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire the [`SERIAL_GATE`]; hold the returned guard for the whole test.
/// Poison-tolerant: a failing (panicking) test must not cascade-poison the
/// gate and spuriously fail the others. For `#[test]` (sync) tests only.
pub(crate) fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    SERIAL_GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Use the freshly built test binary to avoid stale release output formats.
pub(crate) fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agent-gossip"))
}

/// Per-test-process log dir so `cargo task test` never writes into
/// the operator's default `agent-gossip/logs`. Passed via the
/// global `--log-dir` flag.
pub(crate) fn test_log_dir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("agent-gossip-test-logs-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir.to_string_lossy().into_owned()
    })
}

/// `Command` for the built binary with `--log-dir` redirected to a
/// per-test temp dir. Use instead of `Command::new(bin())`. (`--log-dir`
/// is a global flag, so it sits before the subcommand here.)
pub(crate) fn test_cmd() -> Command {
    let mut cmd = Command::new(bin());
    cmd.arg("--log-dir").arg(test_log_dir());
    cmd
}

/// Apply `(flag, value)` tuning pairs to a spawn `Command`. `RUST_LOG` (a
/// kept standard convention, not app config) is set as an **env var**; every
/// other pair becomes a CLI flag (empty value ⇒ bare flag, e.g.
/// `("--directory-private", "")`). Use for the `Node` spawns that may carry a
/// `RUST_LOG` debug filter.
fn apply_flags(cmd: &mut Command, pairs: &[(&str, &str)]) {
    for (flag, value) in pairs {
        if *flag == "RUST_LOG" {
            cmd.env("RUST_LOG", value);
        } else {
            cmd.arg(flag);
            if !value.is_empty() {
                cmd.arg(value);
            }
        }
    }
}

/// Turn `(flag, value)` tuning pairs into CLI args for a spawned `agent-gossip`
/// (replaces the former `.envs(...)` overrides). An empty value yields a
/// bare flag — e.g. the boolean `("--directory-private", "")`. For pair lists
/// that never include `RUST_LOG` (directory / monitor spawns); use
/// [`apply_flags`] otherwise.
pub(crate) fn flag_args(pairs: &[(&str, &str)]) -> Vec<String> {
    pairs
        .iter()
        .flat_map(|(flag, value)| {
            let mut out = vec![(*flag).to_string()];
            if !value.is_empty() {
                out.push((*value).to_string());
            }
            out
        })
        .collect()
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

pub(crate) fn tmp_log(tag: &str) -> PathBuf {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "agent-gossip-test-{}-{}-{}.log",
        tag,
        std::process::id(),
        sequence
    ))
}

pub(crate) fn socket_path(swarm: &str, nickname: &str) -> String {
    format!(
        "{RUNTIME_DIR}/{}/{nickname}.ipc.sock",
        agent_gossip::swarm_prefix(swarm)
    )
}

/// A node's tracing-sink log (distinct from its captured stdout/stderr
/// in `Node::log`). Mirrors `agent_gossip::logs::log_file_path`:
/// `<swarm_prefix>/<nick>.tracing.log` under the per-test log dir. Use this to
/// assert on `tracing` output (warn/info) the operator stream never carries.
pub(crate) fn trace_log(swarm: &str, nickname: &str) -> String {
    let path = format!(
        "{}/{}/{nickname}.tracing.log",
        test_log_dir(),
        agent_gossip::swarm_prefix(swarm)
    );
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

/// Spawn `agent-gossip msg …` and return the raw `Output`
/// (no success assertion — callers that test failure paths inspect it).
pub(crate) fn cli_msg_raw(swarm: &str, nickname: &str, body: &str, reply: Option<&str>) -> Output {
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
    test_cmd()
        .args(&args)
        .output()
        .expect("msg command failed to spawn")
}

/// `cli_msg_raw` + trim stdout. No success assertion.
pub(crate) fn cli_msg_stdout(
    swarm: &str,
    nickname: &str,
    body: &str,
    reply: Option<&str>,
) -> String {
    let out = cli_msg_raw(swarm, nickname, body, reply);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `cli_msg_raw` + assert success + trim stdout.
pub(crate) fn cli_msg_checked(
    swarm: &str,
    nickname: &str,
    body: &str,
    reply: Option<&str>,
) -> String {
    let out = cli_msg_raw(swarm, nickname, body, reply);
    assert!(
        out.status.success(),
        "msg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Spawn `agent-gossip notice …`, assert success, trim stdout. The notice
/// counterpart of [`cli_msg_checked`].
pub(crate) fn cli_notice(swarm: &str, nickname: &str, body: &str, reply: Option<&str>) -> String {
    let mut args = vec![
        "notice",
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
    let out = test_cmd()
        .args(&args)
        .output()
        .expect("notice command failed to spawn");
    assert!(
        out.status.success(),
        "notice failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Spawn `agent-gossip poll … --output json`, assert success,
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

/// `agent-gossip poll --long` (long-poll; blocks until events arrive), returning the
/// JSON stdout and how long the call took — so a test can assert it blocked /
/// resolved promptly.
pub(crate) fn cli_poll_long(
    swarm: &str,
    nickname: &str,
    after: Option<&str>,
) -> (String, Duration) {
    let mut args = vec![
        "poll",
        "--swarm",
        swarm,
        "--nickname",
        nickname,
        "--long",
        "--output",
        "json",
    ];
    if let Some(id) = after {
        args.extend(["--after", id]);
    }
    let started = Instant::now();
    let out = test_cmd()
        .args(&args)
        .output()
        .expect("poll --long command failed to spawn");
    let elapsed = started.elapsed();
    assert!(
        out.status.success(),
        "poll --long failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        elapsed,
    )
}

/// Send one raw JSON command line straight to a daemon's Unix socket and
/// return the (trimmed) response line — the wire-contract path, bypassing the
/// `agent-gossip` client entirely (so a test can exercise a single daemon-side
/// long-poll park, which `poll --long` deliberately hides behind its
/// re-issue loop).
pub(crate) fn ipc_raw(swarm: &str, nickname: &str, line: &str) -> String {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path(swarm, nickname))
        .expect("connect to daemon socket");
    stream
        .write_all(format!("{line}\n").as_bytes())
        .expect("write IPC command");
    stream.flush().expect("flush IPC command");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read IPC response");
    response.trim().to_string()
}

/// Spawn `agent-gossip task …` and return the raw `Output` (no success
/// assertion — callers that test the unknown-participant
/// failure paths inspect it).
pub(crate) fn cli_task_raw(
    swarm: &str,
    nickname: &str,
    to: &str,
    task_id: &str,
    phase: &str,
    text: &str,
) -> Output {
    test_cmd()
        .args([
            "task",
            "--swarm",
            swarm,
            "--nickname",
            nickname,
            "--to",
            to,
            "--task-id",
            task_id,
            "--phase",
            phase,
            "--text",
            text,
        ])
        .output()
        .expect("task command failed to spawn")
}

/// `cli_task_raw` + assert success.
pub(crate) fn cli_task_checked(
    swarm: &str,
    nickname: &str,
    to: &str,
    task_id: &str,
    phase: &str,
    text: &str,
) {
    let out = cli_task_raw(swarm, nickname, to, task_id, phase, text);
    assert!(
        out.status.success(),
        "task failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Spawn `agent-gossip peers …`, assert success, return trimmed stdout (the
/// raw `{ok, participants, count}` JSON line).
pub(crate) fn cli_peers(swarm: &str, nickname: &str) -> String {
    let out = test_cmd()
        .args(["peers", "--swarm", swarm, "--nickname", nickname])
        .output()
        .expect("peers command failed to spawn");
    assert!(
        out.status.success(),
        "peers failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Spawn `agent-gossip ping … `, assert success. Fire-and-forget — the RTT
/// report lands on the target daemon's own output stream, not here.
pub(crate) fn cli_ping(swarm: &str, nickname: &str) {
    let out = test_cmd()
        .args(["ping", "--swarm", swarm, "--nickname", nickname])
        .output()
        .expect("ping command failed to spawn");
    assert!(
        out.status.success(),
        "ping failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The CLI subcommand for a channel (`state` / `meta`) — `Channel::label` is
/// `pub(crate)`, not reachable from this external test crate.
pub(crate) fn channel_subcommand(channel: Channel) -> &'static str {
    match channel {
        Channel::State => "state",
        Channel::Meta => "meta",
    }
}

/// Spawn `agent-gossip <channel> get … `, assert success, return trimmed stdout (the
/// raw `{ok, document}` JSON line). Drives the real CLI → IPC socket → daemon
/// read path the embed harness bypasses.
pub(crate) fn cli_channel_get(channel: Channel, swarm: &str, nickname: &str) -> String {
    let out = test_cmd()
        .args([channel_subcommand(channel), "get"])
        .args(["--swarm", swarm, "--nickname", nickname])
        .output()
        .expect("channel get failed to spawn");
    assert!(
        out.status.success(),
        "{} get failed: {}",
        channel_subcommand(channel),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Spawn `agent-gossip <channel> merge …`, returning the raw
/// [`Output`](std::process::Output). The CLI **exits non-zero** on a rejected
/// `{ok:false}` merge (the scriptable exit-code contract), so this returns the
/// status + stdout unjudged for the caller to assert on.
pub(crate) fn cli_channel_merge(
    channel: Channel,
    swarm: &str,
    nickname: &str,
    merge: &str,
) -> Output {
    test_cmd()
        .args([channel_subcommand(channel), "merge"])
        .args(["--swarm", swarm, "--nickname", nickname, "--merge", merge])
        .output()
        .expect("channel merge failed to spawn")
}

// ── In-process harness (embed::SwarmSession) ──────────────────────

use agent_gossip::embed::{CreateConfig, JoinConfig, SwarmSession};
use agent_gossip::{
    Channel, Message, MessageBody, MessageId, MessageKind, Nickname, OutputEvent, PresenceSubtype,
    SwarmName, TaskId, TaskPhase,
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

/// Validate a `&str` into a [`SwarmName`] for the in-process harness.
fn test_swarm_name(name: &str) -> SwarmName {
    SwarmName::new(name).expect("valid test swarm name")
}

impl InProcNode {
    /// Create a new private swarm. `self.swarm` holds the `💬…` id.
    pub(crate) async fn create(name: &str) -> Self {
        Self::from_session(
            SwarmSession::create(CreateConfig::new(test_swarm_name(name)))
                .await
                .expect("in-process create failed"),
        )
    }

    /// Create a new private swarm with an explicit nickname.
    pub(crate) async fn create_with_nick(name: &str, nick: &str) -> Self {
        let mut cfg = CreateConfig::new(test_swarm_name(name));
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        Self::from_session(
            SwarmSession::create(cfg)
                .await
                .expect("in-process create failed"),
        )
    }

    /// Create a new private, password-protected swarm.
    pub(crate) async fn create_with_password(name: &str, password: &str) -> Self {
        let mut cfg = CreateConfig::new(test_swarm_name(name));
        cfg.password = Some(password.to_owned());
        Self::from_session(
            SwarmSession::create(cfg)
                .await
                .expect("in-process passworded create failed"),
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

    /// Join `swarm` (a `💬…` id) with an explicit nickname.
    pub(crate) async fn join(swarm: &str, nickname: &str) -> Self {
        let target = swarm.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        Self::from_session(
            SwarmSession::join(cfg)
                .await
                .expect("in-process join failed"),
        )
    }

    /// Join a password-protected `swarm`, surfacing the join error (so a
    /// wrong-password test can assert on it).
    pub(crate) async fn try_join_with_password(
        swarm: &str,
        nickname: &str,
        password: &str,
    ) -> Result<Self, agent_gossip::embed::JoinError> {
        let target = swarm.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        cfg.password = Some(password.to_owned());
        Ok(Self::from_session(SwarmSession::join(cfg).await?))
    }

    /// Broadcast a plain message; returns the new message id.
    pub(crate) async fn send(&self, text: &str) -> MessageId {
        self.send_to(None, text)
            .await
            .expect("in-process send failed")
    }

    /// Apply an RFC 7386 merge to the shared state. Panics if the merge is
    /// rejected (not a JSON object / loop stopped) — use
    /// [`Self::try_state_merge`] to exercise rejection deliberately.
    pub(crate) async fn state_merge(&self, merge: serde_json::Value) {
        self.try_state_merge(merge)
            .await
            .expect("in-process state_merge failed");
    }

    /// Like [`Self::state_merge`] but returns the raw result.
    pub(crate) async fn try_state_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        self.session.state_merge(merge).await
    }

    /// The current derived shared-state document (the merge fold over the
    /// state log).
    pub(crate) async fn state_get(&self) -> serde_json::Value {
        self.session
            .state_get()
            .await
            .expect("in-process state_get failed")
    }

    /// Apply an RFC 7386 merge to the `meta` channel. Panics on rejection —
    /// use [`Self::try_meta_merge`] to exercise rejection deliberately.
    pub(crate) async fn meta_merge(&self, merge: serde_json::Value) {
        self.try_meta_merge(merge)
            .await
            .expect("in-process meta_merge failed");
    }

    /// Like [`Self::meta_merge`] but returns the raw result.
    pub(crate) async fn try_meta_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        self.session.meta_merge(merge).await
    }

    /// The current derived `meta`-channel document.
    pub(crate) async fn meta_get(&self) -> serde_json::Value {
        self.session
            .meta_get()
            .await
            .expect("in-process meta_get failed")
    }

    // ── channel-parameterized dispatch ──
    // The behavioral tests run against both channels by selecting one at the
    // call site; these forward to the `state_*` / `meta_*` twins above so a
    // single test body covers `Channel::State` and `Channel::Meta`.

    /// Apply a merge to `channel`. Panics on rejection.
    pub(crate) async fn merge(&self, channel: Channel, merge: serde_json::Value) {
        match channel {
            Channel::State => self.state_merge(merge).await,
            Channel::Meta => self.meta_merge(merge).await,
        }
    }

    /// Apply a merge to `channel`, returning the raw result.
    pub(crate) async fn try_merge(
        &self,
        channel: Channel,
        merge: serde_json::Value,
    ) -> anyhow::Result<()> {
        match channel {
            Channel::State => self.try_state_merge(merge).await,
            Channel::Meta => self.try_meta_merge(merge).await,
        }
    }

    /// The current derived document for `channel`.
    pub(crate) async fn get(&self, channel: Channel) -> serde_json::Value {
        match channel {
            Channel::State => self.state_get().await,
            Channel::Meta => self.meta_get().await,
        }
    }

    /// Captured changes on `channel` so far, each `(derived document, is_self)`.
    pub(crate) fn changes(&mut self, channel: Channel) -> Vec<(serde_json::Value, bool)> {
        self.pump();
        self.drained
            .iter()
            .filter_map(|event| match event {
                OutputEvent::StateChanged {
                    channel: chan,
                    document,
                    is_self,
                    ..
                } if *chan == channel => Some((document.clone(), *is_self)),
                _ => None,
            })
            .collect()
    }

    /// Wait until a change on `channel` **from a peer** (`is_self == false`)
    /// whose freshly-derived document satisfies `pred` is captured — the
    /// reaction-hook check (a self-change never satisfies it, exercising the F5
    /// self-wake guard).
    pub(crate) async fn wait_change(
        &mut self,
        channel: Channel,
        timeout: Duration,
        mut pred: impl FnMut(&serde_json::Value) -> bool,
    ) -> bool {
        self.wait_for(timeout, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::StateChanged { channel: chan, document, is_self: false, .. }
                        if *chan == channel && pred(document)
                )
            })
        })
        .await
    }

    /// Send a message addressed to `target`; returns the new id.
    pub(crate) async fn reply(&self, target: &str, text: &str) -> MessageId {
        self.send_to(Some(target), text)
            .await
            .expect("in-process reply failed")
    }

    /// Broadcast a notice (the no-auto-reply kind); `target` directs it
    /// to one peer, like [`Self::reply`].
    pub(crate) async fn notice(&self, target: Option<&str>, text: &str) -> MessageId {
        let reply = target.map(|nick| Nickname::new(nick).expect("valid target nickname"));
        let sent = self
            .session
            .send_notice(MessageBody::new(text).expect("valid body"), reply)
            .await
            .expect("in-process notice failed");
        sent.id
    }

    /// Shared send path for [`Self::send`] and [`Self::reply`]:
    /// `target` `None` is an open broadcast, `Some` a directed reply.
    async fn send_to(&self, target: Option<&str>, text: &str) -> anyhow::Result<MessageId> {
        let reply = target.map(|nick| Nickname::new(nick).expect("valid target nickname"));
        // `send` returns the canonical `Message`; the harness only needs its id.
        let sent = self
            .session
            .send(MessageBody::new(text).expect("valid body"), reply)
            .await?;
        Ok(sent.id)
    }

    /// Send one task leg to `target`, correlated by `task_id`; returns the
    /// new id. Panics on transport error — addressee validation is
    /// `broadcast_task`'s job. (Returns `Option` purely for caller
    /// ergonomics; a successful leg is always `Some`.)
    pub(crate) async fn task(
        &self,
        target: &str,
        task_id: &TaskId,
        phase: TaskPhase,
        text: &str,
    ) -> Option<MessageId> {
        let to = Nickname::new(target).expect("valid target nickname");
        let sent = self
            .session
            .task(
                to,
                task_id.clone(),
                phase,
                MessageBody::new(text).expect("valid body"),
            )
            .await
            .expect("in-process task failed");
        Some(sent.id)
    }

    /// Captured task legs (any phase; includes self echoes — filter on
    /// `is_self`/author as needed).
    pub(crate) fn tasks(&mut self) -> Vec<(&Message, bool)> {
        self.pump();
        self.drained
            .iter()
            .filter_map(|event| match event {
                OutputEvent::Task { msg, is_self } => Some((&**msg, *is_self)),
                _ => None,
            })
            .collect()
    }

    /// Wait until a task leg of `phase` (from any author) is surfaced.
    pub(crate) async fn wait_task(&mut self, phase: TaskPhase, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::Task { msg, .. }
                        if matches!(&msg.kind, MessageKind::Task { phase: got, .. } if *got == phase)
                )
            })
        })
        .await
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
            .filter_map(agent_gossip::event_json)
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

// ── Subprocess harness (real `agent-gossip` processes) ─────
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
    /// Spawn `agent-gossip create`, wait for 💬... and the assigned nickname.
    pub(crate) fn create() -> (Self, String) {
        Self::create_named("itest")
    }

    /// Spawn `agent-gossip create --name <name>`. Uses a fixed name by default
    /// since tests don't care what the swarm is called — only that creation
    /// and join round-trip.
    pub(crate) fn create_named(name: &str) -> (Self, String) {
        Self::create_flags(name, &[])
    }

    /// Like [`create_named`](Self::create_named) but passes extra hidden
    /// tuning flags to the spawned daemon as `(flag, value)` pairs (e.g. a
    /// shortened heal cadence). Replaces the former env overrides.
    pub(crate) fn create_flags(name: &str, flags: &[(&str, &str)]) -> (Self, String) {
        Self::create_args(name, &[], flags)
    }

    /// Like [`create_flags`](Self::create_flags) but also passes extra raw
    /// `create` CLI args (e.g. `["--public"]`).
    pub(crate) fn create_args(
        name: &str,
        extra: &[&str],
        flags: &[(&str, &str)],
    ) -> (Self, String) {
        let log = tmp_log("create");
        let file = File::create(&log).unwrap();
        let mut args = vec!["create", "--name", name];
        args.extend_from_slice(extra);
        let mut cmd = test_cmd();
        cmd.args(&args);
        apply_flags(&mut cmd, flags);
        let child = cmd
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
                // Human-mode create prints `others can join with: agent-gossip
                // join <id>`; pull the id token out of that hint.
                if swarm_id.is_none()
                    && let Some((_, after)) = trimmed.split_once("agent-gossip join ")
                {
                    swarm_id = after.split_whitespace().next().map(str::to_owned);
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

    /// Spawn `agent-gossip join <swarm> --nickname <nickname>`.
    pub(crate) fn join(swarm: &str, nickname: &str) -> Self {
        Self::join_flags(swarm, nickname, &[])
    }

    /// Like [`join`](Self::join) but passes extra hidden tuning flags to the
    /// spawned daemon as `(flag, value)` pairs.
    pub(crate) fn join_flags(swarm: &str, nickname: &str, flags: &[(&str, &str)]) -> Self {
        Self::join_args(swarm, nickname, &[], flags)
    }

    /// Like [`join_flags`](Self::join_flags) but also passes extra raw
    /// `join` CLI args (e.g. `["--password=pw"]`).
    pub(crate) fn join_args(
        swarm: &str,
        nickname: &str,
        extra: &[&str],
        flags: &[(&str, &str)],
    ) -> Self {
        let log = tmp_log(nickname);
        let file = File::create(&log).unwrap();
        let mut cmd = test_cmd();
        cmd.args(["join", swarm, "--nickname", nickname]);
        cmd.args(extra);
        apply_flags(&mut cmd, flags);
        let child = cmd
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

    /// How many **distinct** messages from `author` have a body starting with
    /// `prefix` — the convergence metric for the anti-entropy gap-recovery
    /// tests (each sends `prefix-{i}` and waits for the full distinct set).
    pub(crate) fn count_distinct_from(&self, author: &str, prefix: &str) -> usize {
        self.messages()
            .iter()
            .filter(|msg| msg.author == author && msg.body.starts_with(prefix))
            .map(|msg| msg.body.clone())
            .collect::<std::collections::HashSet<_>>()
            .len()
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

/// `cli_msg_raw` with no success assertion — the gossip tests' default.
pub(crate) fn cli_message_raw(swarm: &str, nickname: &str, body: &str) -> Output {
    cli_msg_raw(swarm, nickname, body, None)
}

/// `cli_msg_stdout` shorthand.
pub(crate) fn cli_message(swarm: &str, nickname: &str, body: &str) -> String {
    cli_msg_stdout(swarm, nickname, body, None)
}

/// `wait_until` with the standard message-delivery timeout.
pub(crate) fn wait_total(total_fn: impl Fn() -> usize, target: usize) -> usize {
    wait_until(total_fn, target, MSG_TIMEOUT)
}
