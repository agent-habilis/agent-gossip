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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

// The single source of truth for the runtime base dir lives in the shared
// crate (a dev-dependency); re-export it so test code resolves the same
// per-user base the daemon uses without a divergent copy.
pub(crate) use agent_square::runtime_base;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_mins(1);
/// Steady-state delivery budget: how long a meshed peer may take to surface a
/// message/presence/task leg. The suite-wide standard for every positive
/// (adaptive, break-on-success) delivery wait. A meshed in-process round trip
/// is normally sub-second; the headroom is for a loaded CI host where two
/// concurrent in-process meshes can stall a delivery well past a tighter
/// budget (crypto-heavy deps are release-optimized even in dev builds via the
/// `[profile.dev.package]` overrides, so debug crypto is no longer the
/// bottleneck). `wait_for`/`wait_until` are adaptive, so a healthy run returns
/// immediately and only a genuine stall pays the ceiling.
pub(crate) const MSG_TIMEOUT: Duration = Duration::from_mins(1);
pub(crate) const POLL: Duration = Duration::from_millis(250);

/// Budget for a delivery asserted **after a disruption** (beacon death,
/// SIGSTOP freeze, rendezvous migration, creator departure). Re-meshing waits
/// on the heal cadence (production 15s; the reliability tests inject a short
/// `--heal-interval-secs`), so a recovery that just missed a tick needs
/// another cycle — the steady-state `MSG_TIMEOUT` (1 min) sits right on that
/// cliff and flakes on a loaded host. 2 min clears many cycles; `wait_until`
/// is adaptive, so a healthy run returns in seconds and only a genuine stall
/// pays the ceiling. The extra headroom over a bare "few cycles" is for a host
/// running real mesh daemons alongside the suite (dogfooding) — CPU starvation
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
    PathBuf::from(env!("CARGO_BIN_EXE_agent-square"))
}

/// Per-test-process log dir so `cargo task test` never writes into
/// the operator's default `agent-square/logs`. Passed via the
/// global `--log-dir` flag.
pub(crate) fn test_log_dir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("agent-square-test-logs-{}", std::process::id()));
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

/// Turn `(flag, value)` tuning pairs into CLI args for a spawned `agent-square`
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
        "agent-square-test-{}-{}-{}.log",
        tag,
        std::process::id(),
        sequence
    ))
}

/// A fresh, unique scratch directory for a spool test, under the OS temp dir.
/// Not pre-created — `spool::install` (via `--spool`) makes it.
pub(crate) fn spool_dir(tag: &str) -> PathBuf {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "agent-square-spool-{}-{}-{}",
        tag,
        std::process::id(),
        sequence
    ))
}

/// The per-mesh subdir a spool writes frames into (`<root>/<mesh-prefix>/`),
/// mirroring `transport::spool::install`.
pub(crate) fn spool_mesh_dir(root: &Path, mesh: &str) -> PathBuf {
    root.join(agent_square::mesh_prefix(mesh))
}

/// Poll until the spool subdir holds at least `min` committed `.frame` files,
/// or `timeout` elapses. Returns the count seen (so a caller can assert `>=`).
pub(crate) async fn wait_for_frames(dir: &Path, min: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        let count = fs::read_dir(dir).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "frame"))
                .count()
        });
        if count >= min || Instant::now() >= deadline {
            return count;
        }
        tokio::time::sleep(POLL).await;
    }
}

pub(crate) fn socket_path(mesh: &str, nickname: &str) -> String {
    runtime_base()
        .join(agent_square::mesh_prefix(mesh))
        .join(format!("{nickname}.ipc.sock"))
        .display()
        .to_string()
}

/// A node's tracing-sink log (distinct from its captured stdout/stderr
/// in `Node::log`). Mirrors `agent_square::logs::log_file_path`:
/// `<mesh_prefix>/<nick>.tracing.log` under the per-test log dir. Use this to
/// assert on `tracing` output (warn/info) the operator stream never carries.
pub(crate) fn trace_log(mesh: &str, nickname: &str) -> String {
    let path = format!(
        "{}/{}/{nickname}.tracing.log",
        test_log_dir(),
        agent_square::mesh_prefix(mesh)
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

/// Spawn `agent-square msg …` and return the raw `Output`
/// (no success assertion — callers that test failure paths inspect it).
/// Broadcast a mesh chat message via the CLI — `agent-square a2a call --method
/// message/send --text` with no `--to` (A2A is point-to-point, so a mesh-wide
/// message declares itself).
pub(crate) fn cli_msg_raw(mesh: &str, nickname: &str, body: &str) -> Output {
    test_cmd()
        .args([
            "a2a",
            "call",
            "--square",
            mesh,
            "--nickname",
            nickname,
            "--method",
            "SendMessage",
            "--text",
            body,
        ])
        .output()
        .expect("a2a call command failed to spawn")
}

/// `cli_msg_raw` + trim stdout. No success assertion.
pub(crate) fn cli_msg_stdout(mesh: &str, nickname: &str, body: &str) -> String {
    let out = cli_msg_raw(mesh, nickname, body);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `cli_msg_raw` + assert success + trim stdout.
pub(crate) fn cli_msg_checked(mesh: &str, nickname: &str, body: &str) -> String {
    let out = cli_msg_raw(mesh, nickname, body);
    assert!(
        out.status.success(),
        "a2a call (broadcast) failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Spawn `agent-square poll … --output json`, assert success,
/// return trimmed stdout.
pub(crate) fn cli_poll(mesh: &str, nickname: &str, after: Option<&str>) -> String {
    let mut args = vec![
        "poll",
        "--square",
        mesh,
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

/// `agent-square poll --long` (long-poll; blocks until events arrive), returning the
/// JSON stdout and how long the call took — so a test can assert it blocked /
/// resolved promptly.
pub(crate) fn cli_poll_long(mesh: &str, nickname: &str, after: Option<&str>) -> (String, Duration) {
    let mut args = vec![
        "poll",
        "--square",
        mesh,
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
/// `agent-square` client entirely (so a test can exercise a single daemon-side
/// long-poll park, which `poll --long` deliberately hides behind its
/// re-issue loop).
pub(crate) fn ipc_raw(mesh: &str, nickname: &str, line: &str) -> String {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path(mesh, nickname))
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

/// Create a task on `to` via the CLI (`agent-square a2a call --to <peer> --method
/// message/send --text`) and return the raw `Output` (no success assertion —
/// callers that test the unknown-participant failure path inspect it).
pub(crate) fn cli_task_create_raw(mesh: &str, nickname: &str, to: &str, text: &str) -> Output {
    test_cmd()
        .args([
            "a2a",
            "call",
            "--square",
            mesh,
            "--nickname",
            nickname,
            "--to",
            to,
            "--method",
            "SendMessage",
            "--text",
            text,
        ])
        .output()
        .expect("a2a call command failed to spawn")
}

/// Create a task and return the **worker-minted** task id (parsed from the
/// JSON-RPC response the call prints on stdout). Panics on failure.
pub(crate) fn cli_task_create(mesh: &str, nickname: &str, to: &str, text: &str) -> String {
    let out = cli_task_create_raw(mesh, nickname, to, text);
    assert!(
        out.status.success(),
        "a2a create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("a2a call prints a JSON-RPC response");
    parsed["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task id")
        .to_string()
}

/// Worker-emit a task status via the CLI (`agent-square a2a status`). Panics on failure.
pub(crate) fn cli_task_status(mesh: &str, nickname: &str, task_id: &str, state: &str) {
    let out = test_cmd()
        .args([
            "a2a",
            "status",
            "--square",
            mesh,
            "--nickname",
            nickname,
            "--task-id",
            task_id,
            "--state",
            state,
        ])
        .output()
        .expect("a2a status command failed to spawn");
    assert!(
        out.status.success(),
        "a2a status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Spawn `agent-square peers …`, assert success, return trimmed stdout (the
/// raw `{ok, participants, count}` JSON line).
pub(crate) fn cli_peers(mesh: &str, nickname: &str) -> String {
    let out = test_cmd()
        .args(["peers", "--square", mesh, "--nickname", nickname])
        .output()
        .expect("peers command failed to spawn");
    assert!(
        out.status.success(),
        "peers failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Spawn `agent-square ping … `, assert success. Fire-and-forget — the RTT
/// report lands on the target daemon's own output stream, not here.
pub(crate) fn cli_ping(mesh: &str, nickname: &str) {
    let out = test_cmd()
        .args(["ping", "--square", mesh, "--nickname", nickname])
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

/// Spawn `agent-square <channel> get … `, assert success, return trimmed stdout (the
/// raw `{ok, document}` JSON line). Drives the real CLI → IPC socket → daemon
/// read path the embed harness bypasses.
pub(crate) fn cli_channel_get(channel: Channel, mesh: &str, nickname: &str) -> String {
    let out = test_cmd()
        .args([channel_subcommand(channel), "get"])
        .args(["--square", mesh, "--nickname", nickname])
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

/// Spawn `agent-square <channel> merge …`, returning the raw
/// [`Output`](std::process::Output). The CLI **exits non-zero** on a rejected
/// `{ok:false}` merge (the scriptable exit-code contract), so this returns the
/// status + stdout unjudged for the caller to assert on.
pub(crate) fn cli_channel_merge(
    channel: Channel,
    mesh: &str,
    nickname: &str,
    merge: &str,
) -> Output {
    test_cmd()
        .args([channel_subcommand(channel), "merge"])
        .args(["--square", mesh, "--nickname", nickname, "--merge", merge])
        .output()
        .expect("channel merge failed to spawn")
}

// ── In-process harness (embed::MeshSession) ──────────────────────

use agent_square::embed::{
    A2aCallParams, CreateConfig, JoinConfig, MeshSession, TaskArtifactParams,
};
use agent_square::{
    Channel, MeshName, Message, MessageBody, MessageId, MessageKind, Nickname, OutputEvent,
    PresenceSubtype, TaskId, TaskState, TransportPolicy,
};
use tokio::sync::mpsc::UnboundedReceiver;

/// One in-process mesh node: a real [`MeshSession`] (real iroh
/// endpoint + the real `daemon::run` loop on a background task) plus
/// its captured [`OutputEvent`] stream. Drop-in analogue of the
/// subprocess `JsonNode`/`Node`, but everything runs in the test
/// process — so coverage is recorded and teardown is deterministic.
pub(crate) struct InProcNode {
    pub session: MeshSession,
    rx: UnboundedReceiver<OutputEvent>,
    drained: Vec<OutputEvent>,
    pub mesh: String,
    pub nickname: String,
}

/// Validate a `&str` into a [`MeshName`] for the in-process harness.
fn test_mesh_name(name: &str) -> MeshName {
    MeshName::new(name).expect("valid test mesh name")
}

impl InProcNode {
    /// Create a new private mesh. `self.mesh` holds the `💬…` id.
    pub(crate) async fn create(name: &str) -> Self {
        Self::from_session(
            MeshSession::create(CreateConfig::new(test_mesh_name(name)))
                .await
                .expect("in-process create failed"),
        )
    }

    /// Create a new private mesh with an explicit nickname.
    pub(crate) async fn create_with_nick(name: &str, nick: &str) -> Self {
        let mut cfg = CreateConfig::new(test_mesh_name(name));
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        Self::from_session(
            MeshSession::create(cfg)
                .await
                .expect("in-process create failed"),
        )
    }

    /// Create a new private, password-protected mesh.
    pub(crate) async fn create_with_password(name: &str, password: &str) -> Self {
        let mut cfg = CreateConfig::new(test_mesh_name(name));
        cfg.password = Some(password.to_owned());
        Self::from_session(
            MeshSession::create(cfg)
                .await
                .expect("in-process passworded create failed"),
        )
    }

    /// Create a mesh that mirrors every frame into `spool` and ingests frames
    /// other daemons write there (the filesystem spool, `--spool`).
    pub(crate) async fn create_with_spool(name: &str, nick: &str, spool: &Path) -> Self {
        let mut cfg = CreateConfig::new(test_mesh_name(name));
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        cfg.spool = Some(spool.to_path_buf());
        Self::from_session(
            MeshSession::create(cfg)
                .await
                .expect("in-process spool create failed"),
        )
    }

    /// Join `mesh` (a `💬…` id), mirroring/ingesting frames through `spool`.
    pub(crate) async fn join_with_spool(mesh: &str, nick: &str, spool: &Path) -> Self {
        let target = mesh.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        cfg.spool = Some(spool.to_path_buf());
        Self::from_session(
            MeshSession::join(cfg)
                .await
                .expect("in-process spool join failed"),
        )
    }

    /// Create a mesh under an explicit transport policy + active-view cap — the
    /// per-node knobs the transport matrix forces (e.g. `no_unicast` + a
    /// `max_peers = 1` line topology to route directed traffic over a circuit).
    pub(crate) async fn create_with_transport(
        name: &str,
        nick: &str,
        transport: TransportPolicy,
        max_peers: usize,
    ) -> Self {
        let mut cfg = CreateConfig::new(test_mesh_name(name));
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        cfg.transport = transport;
        cfg.max_peers = max_peers;
        Self::from_session(
            MeshSession::create(cfg)
                .await
                .expect("in-process transport create failed"),
        )
    }

    /// Join `mesh` under an explicit transport policy + active-view cap.
    pub(crate) async fn join_with_transport(
        mesh: &str,
        nick: &str,
        transport: TransportPolicy,
        max_peers: usize,
    ) -> Self {
        let target = mesh.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        cfg.transport = transport;
        cfg.max_peers = max_peers;
        Self::from_session(
            MeshSession::join(cfg)
                .await
                .expect("in-process transport join failed"),
        )
    }

    fn from_session(mut session: MeshSession) -> Self {
        let rx = session.events().expect("events() receiver");
        let mesh = session.mesh_id().to_string();
        let nickname = session.nickname().to_string();
        Self {
            session,
            rx,
            drained: Vec::new(),
            mesh,
            nickname,
        }
    }

    /// Join `mesh` (a `💬…` id) with an explicit nickname.
    pub(crate) async fn join(mesh: &str, nickname: &str) -> Self {
        let target = mesh.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        Self::from_session(
            MeshSession::join(cfg)
                .await
                .expect("in-process join failed"),
        )
    }

    /// Join a password-protected `mesh`, surfacing the join error (so a
    /// wrong-password test can assert on it).
    pub(crate) async fn try_join_with_password(
        mesh: &str,
        nickname: &str,
        password: &str,
    ) -> Result<Self, agent_square::embed::JoinError> {
        let target = mesh.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        cfg.password = Some(password.to_owned());
        Ok(Self::from_session(MeshSession::join(cfg).await?))
    }

    /// Broadcast a plain message; returns the new message id.
    pub(crate) async fn send(&self, text: &str) -> MessageId {
        self.session
            .send(MessageBody::new(text).expect("valid body"))
            .await
            .expect("in-process send failed")
            .id
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

    /// Create a task on `target` (native A2A `message/send`, no `taskId`): the
    /// worker mints the id and returns the `Task`. Returns the parsed JSON-RPC
    /// response.
    pub(crate) async fn create_task(&self, target: &str, text: &str) -> serde_json::Value {
        // Task creation seals the request to `target`'s card
        // (`a2a::card::peer_seal_key`). The card propagates asynchronously and the
        // seal is one-shot, so wait for it first — otherwise, under the concurrent
        // suite's load, the request is silently dropped and the call hangs.
        self.await_peer_card(target).await;
        let msg =
            agent_square::a2a::gossip::send_message_payload(self.session.mesh_id(), None, text);
        self.a2a_call(target, "SendMessage", serde_json::json!({ "message": msg }))
            .await
    }

    /// Block until `target`'s card is present in this node's `meta` doc (the
    /// seal key source for any directed A2A call), or panic on timeout.
    pub(crate) async fn await_peer_card(&self, target: &str) {
        let pointer = format!("/peers/{target}/card");
        let deadline = Instant::now() + MSG_TIMEOUT;
        while self.meta_get().await.pointer(&pointer).is_none() {
            assert!(
                Instant::now() < deadline,
                "{target}'s card never reached this node's meta doc"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    /// Send a follow-up message (answer / approval / change) into an existing
    /// task on `target`. Returns the updated `Task` response.
    pub(crate) async fn task_message(
        &self,
        target: &str,
        task_id: &TaskId,
        text: &str,
    ) -> serde_json::Value {
        let msg = agent_square::a2a::gossip::send_message_payload(
            self.session.mesh_id(),
            Some(task_id),
            text,
        );
        self.a2a_call(target, "SendMessage", serde_json::json!({ "message": msg }))
            .await
    }

    /// A directed A2A call to `target`; returns the parsed JSON-RPC response.
    pub(crate) async fn a2a_call(
        &self,
        target: &str,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        self.session
            .a2a_call(A2aCallParams {
                peer: Nickname::new(target).expect("valid target"),
                method: method.to_owned(),
                params,
                timeout: Duration::from_mins(1),
            })
            .await
            .expect("a2a_call transport")
    }

    /// Worker-emit a `TaskStatusUpdate` on a task we're serving.
    pub(crate) async fn task_status(&self, task_id: &TaskId, state: TaskState, note: Option<&str>) {
        self.session
            .task_status(task_id.clone(), state, note.map(str::to_owned))
            .await
            .expect("in-process task_status failed");
    }

    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task we're serving.
    pub(crate) async fn task_artifact(&self, task_id: &TaskId, text: &str) {
        self.session
            .task_artifact(TaskArtifactParams {
                task_id: task_id.clone(),
                text: text.to_owned(),
                file: None,
                file_name: None,
                file_mime: None,
            })
            .await
            .expect("in-process task_artifact failed");
    }

    /// Worker-emit a `TaskArtifactUpdate` whose result is a file, offloaded over
    /// the blob channel and referenced by a `Part.url`. Returns the daemon's echo
    /// so a test can read the minted `💬…` reference.
    pub(crate) async fn task_artifact_file(&self, task_id: &TaskId, path: &Path) -> Message {
        self.session
            .task_artifact(TaskArtifactParams {
                task_id: task_id.clone(),
                text: String::new(),
                file: Some(path.to_path_buf()),
                file_name: None,
                file_mime: None,
            })
            .await
            .expect("in-process task_artifact_file failed")
    }

    /// Captured worker-pushed task frames (status/artifact; includes self
    /// echoes — filter on `is_self`/author as needed).
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

    /// Whether any `message`-kind task event (an RPC `message/send` leg) has
    /// been surfaced so far.
    pub(crate) fn saw_task_message(&mut self) -> bool {
        self.pump();
        self.drained
            .iter()
            .any(|event| matches!(event, OutputEvent::TaskMessage { .. }))
    }

    /// Wait until a worker-pushed task frame in `state` (from any author) is
    /// surfaced.
    pub(crate) async fn wait_task_state(&mut self, state: TaskState, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::Task { msg, .. }
                        if agent_square::a2a::gossip::frame_task_state(msg) == Some(state)
                )
            })
        })
        .await
    }

    /// Wait until a `message`-kind task event (an RPC `message/send` leg) is
    /// surfaced to us (the worker).
    pub(crate) async fn wait_task_message(&mut self, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events
                .iter()
                .any(|event| matches!(event, OutputEvent::TaskMessage { .. }))
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
            .filter_map(agent_square::event_json)
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
                    OutputEvent::Message { msg, is_self: false }
                        if agent_square::a2a::gossip::chat_text(msg).as_deref() == Some(body)
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
                    OutputEvent::Message { msg, is_self: false }
                        if agent_square::a2a::gossip::chat_text(msg).as_deref() == Some(body)
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
/// process. The mesh id is `creator.mesh`.
pub(crate) async fn three_peers(suffix: &str) -> (InProcNode, InProcNode, InProcNode) {
    let creator = InProcNode::create(&format!("mon{suffix}")).await;
    let joiner_a = InProcNode::join(&creator.mesh, &format!("mon-{suffix}-a")).await;
    let joiner_b = InProcNode::join(&creator.mesh, &format!("mon-{suffix}-b")).await;
    (creator, joiner_a, joiner_b)
}

// ── Subprocess harness (real `agent-square` processes) ─────
//
// For the reliability / contract tests that must exercise the shipped
// binary: real SIGKILL / SIGSTOP-SIGCONT, real stdout, real
// Unix-socket IPC, real heal/anti-entropy timing.

/// Human-mode create prints `others can join with: agent-square join <id>`;
/// pull the id token out of that hint, if `trimmed` is such a line.
fn parse_join_hint_mesh_id(trimmed: &str) -> Option<String> {
    let (_, after) = trimmed.split_once("agent-square join ")?;
    after.split_whitespace().next().map(str::to_owned)
}

/// Both lifecycle lines end with ` as <NICK>` (`created #N and joined as
/// <nick>` / `joined #N as <nick>`); presence/message lines never contain
/// ` as <`. Anchor on the last ` as <` so the leading `<author>` of a
/// message line can't be mistaken for the nick.
fn parse_lifecycle_nickname(trimmed: &str) -> Option<String> {
    let (_, after_as) = trimmed.rsplit_once(" as <")?;
    let end = after_as.find('>')?;
    Some(after_as[..end].to_string())
}

/// Scan a `create`/`join` log's full contents so far for the mesh id and
/// nickname, filling in whichever of `mesh_id`/`nickname` is still unset.
fn scan_create_log(content: &str, mesh_id: &mut Option<String>, nickname: &mut Option<String>) {
    for line in content.lines() {
        let trimmed = line.trim();
        if mesh_id.is_none() {
            *mesh_id = parse_join_hint_mesh_id(trimmed);
        }
        if nickname.is_none() {
            *nickname = parse_lifecycle_nickname(trimmed);
        }
    }
}

pub(crate) struct Node {
    child: Child,
    log: PathBuf,
    pub nickname: String,
}

impl Node {
    /// Spawn `agent-square create`, wait for 💬... and the assigned nickname.
    pub(crate) fn create() -> (Self, String) {
        Self::create_named("itest")
    }

    /// Spawn `agent-square create --name <name>`. Uses a fixed name by default
    /// since tests don't care what the mesh is called — only that creation
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

    /// Spawn `agent-square join <mesh> --nickname <nickname>`.
    pub(crate) fn join(mesh: &str, nickname: &str) -> Self {
        Self::join_flags(mesh, nickname, &[])
    }

    /// Like [`join`](Self::join) but passes extra hidden tuning flags to the
    /// spawned daemon as `(flag, value)` pairs.
    pub(crate) fn join_flags(mesh: &str, nickname: &str, flags: &[(&str, &str)]) -> Self {
        Self::join_args(mesh, nickname, &[], flags)
    }

    /// Like [`join_flags`](Self::join_flags) but also passes extra raw
    /// `join` CLI args (e.g. `["--password=pw"]`).
    pub(crate) fn join_args(
        mesh: &str,
        nickname: &str,
        extra: &[&str],
        flags: &[(&str, &str)],
    ) -> Self {
        let log = tmp_log(nickname);
        let file = File::create(&log).unwrap();
        let mut cmd = test_cmd();
        cmd.args(["join", mesh, "--nickname", nickname]);
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
    pub(crate) fn wait_ready(&self, mesh: &str) -> bool {
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

    /// Block until the child exits **on its own** (e.g. after
    /// [`sigint`](Self::sigint)). `Drop` SIGKILLs immediately, which
    /// truncates a graceful shutdown mid-flight — the daemon never
    /// finishes its `Left` broadcast + clean QUIC close, leaving peers
    /// a zombie link that takes iroh's idle timeout to die. Reaping
    /// here guarantees the shutdown (including socket release) fully
    /// completed before the test proceeds.
    pub(crate) fn wait_exit(&mut self) {
        let _ = self.child.wait();
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

/// The text projection of a chat frame's A2A payload — what a test asserts
/// against, since the frame `body` carries the serialized payload.
pub(crate) fn chat_text(msg: &Message) -> String {
    agent_square::a2a::gossip::chat_text(msg).expect("a chat frame carries an a2a payload")
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
pub(crate) fn cli_message_raw(mesh: &str, nickname: &str, body: &str) -> Output {
    cli_msg_raw(mesh, nickname, body)
}

/// `cli_msg_stdout` shorthand.
pub(crate) fn cli_message(mesh: &str, nickname: &str, body: &str) -> String {
    cli_msg_stdout(mesh, nickname, body)
}

/// `wait_until` with the standard message-delivery timeout.
pub(crate) fn wait_total(total_fn: impl Fn() -> usize, target: usize) -> usize {
    wait_until(total_fn, target, MSG_TIMEOUT)
}
