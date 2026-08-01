#![expect(
    clippy::wildcard_enum_match_arm,
    reason = "OutputEvent is #[non_exhaustive]; matching it from outside agent-gossip mandates a wildcard arm, so exhaustive enumeration is impossible"
)]
#![expect(
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    reason = "a test harness, not a public API: panicking on a broken invariant IS the assertion, so a `# Panics` section on every helper documents nothing, and no caller benefits from `#[must_use]`"
)]
//! Shared harness for the `agent-gossip` integration suites.
//!
//! This is a library crate, not a `tests/common/` module, for one reason: each
//! integration binary is its own compilation unit, so a shared `mod common;`
//! makes every helper it does not personally call look dead. Sixteen binaries
//! with wildly different needs (`man_contract` uses one helper; `gossip_network`
//! uses two dozen) made that unavoidable, and the blanket `#![allow(dead_code)]`
//! it forced also hid genuinely dead helpers. A library's `pub` surface is
//! exempt from the lint by construction, so the suppression is gone — while the
//! crate's own private helpers stay `dead_code`-checked.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

// The single source of truth for the runtime base dir lives in the app crate;
// re-export it so test code resolves the same per-user base the daemon uses
// without a divergent copy.
pub use agent_gossip::runtime_base;

pub const CONNECT_TIMEOUT: Duration = Duration::from_mins(1);
/// Steady-state delivery budget: how long a meshed peer may take to surface a
/// message/presence/task leg. The suite-wide standard for every positive
/// (adaptive, break-on-success) delivery wait. A meshed in-process round trip
/// is normally sub-second; the headroom is for a loaded CI host where two
/// concurrent in-process meshes can stall a delivery well past a tighter
/// budget (crypto-heavy deps are release-optimized even in dev builds via the
/// `[profile.dev.package]` overrides, so debug crypto is no longer the
/// bottleneck). `wait_for`/`wait_until` are adaptive, so a healthy run returns
/// immediately and only a genuine stall pays the ceiling.
pub const MSG_TIMEOUT: Duration = Duration::from_mins(1);
pub const POLL: Duration = Duration::from_millis(250);

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
pub const RECOVERY_TIMEOUT: Duration = Duration::from_mins(2);

/// Delivery budget for a **big (unlogged) multi-shard body**.
///
/// A large shard group skips the message log by design — it must not evict the
/// anti-entropy history — so anti-entropy cannot heal it. A dropped shard is
/// recovered only by the receiver's `shard/repair` ask, and that path is
/// deliberately lazy: the partial group has to sit idle for
/// `REASSEMBLY_REPAIR_IDLE_SECS` (60s) *and* the check runs on a one-minute
/// prune tick, so a repair round can land anywhere up to ~2 minutes out.
///
/// Asserting a big body's arrival within the steady-state `MSG_TIMEOUT` (1 min)
/// therefore asserts that **no shard is ever dropped** — which the design does
/// not promise; the repair path exists precisely because one can be. A budget
/// that cannot clear a full repair cycle turns a single lost shard into a test
/// failure, which is what made the multi-shard tests flaky. Adaptive like every
/// other wait here: a healthy run returns in well under a second, and only a
/// genuine stall pays the ceiling.
pub const BIG_BODY_TIMEOUT: Duration = Duration::from_mins(3);

/// Per-attempt deadline for a directed A2A call (see [`InProcNode::a2a_call`]).
/// Deliberately far below `MSG_TIMEOUT`: a request the overlay dropped will
/// never be answered, so the only thing a long wait buys is a slower retry. Long
/// enough that a merely *slow* peer still answers within one attempt.
const RPC_ATTEMPT: Duration = Duration::from_secs(10);

/// Whether `response` is the daemon's "nobody ever answered" transport timeout,
/// as opposed to a real reply carrying an error. Only the former is worth
/// re-issuing — see [`InProcNode::a2a_call`].
fn is_rpc_timeout(response: &serde_json::Value) -> bool {
    response["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("timed out"))
}

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
pub fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    SERIAL_GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The freshly built `agent-gossip` binary under test (never a stale release
/// build, whose output formats may differ).
///
/// Resolved from the *running* test executable rather than
/// `env!("CARGO_BIN_EXE_agent-gossip")`: cargo defines that variable only while
/// compiling the owning package's integration tests and benches, never for a
/// library — not even a path dev-dependency of that same package — so the
/// compile-time form cannot live in this crate. libtest puts the test binary at
/// `<target>/<profile>/deps/<name>-<hash>`; the app sits one level up.
pub fn bin() -> PathBuf {
    let mut dir = std::env::current_exe().expect("path of the running test executable");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir.join("agent-gossip")
}

/// Per-test-process log dir so `cargo task test` never writes into
/// the operator's default `agent-gossip/logs`. Passed via the
/// global `--log-dir` flag.
pub fn test_log_dir() -> &'static str {
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
pub fn test_cmd() -> Command {
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
pub fn flag_args(pairs: &[(&str, &str)]) -> Vec<String> {
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

pub fn tmp_log(tag: &str) -> PathBuf {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "agent-gossip-test-{}-{}-{}.log",
        tag,
        std::process::id(),
        sequence
    ))
}

pub fn socket_path(mesh: &str, nickname: &str) -> String {
    runtime_base()
        .join(agent_gossip::mesh_prefix(mesh))
        .join(format!("{nickname}.ipc.sock"))
        .display()
        .to_string()
}

/// A node's tracing-sink log (distinct from its captured stdout/stderr
/// in `Node::log`). Mirrors `agent_gossip::logs::log_file_path`:
/// `<mesh_prefix>/<nick>.tracing.log` under the per-test log dir. Use this to
/// assert on `tracing` output (warn/info) the operator stream never carries.
pub fn trace_log(mesh: &str, nickname: &str) -> String {
    let path = format!(
        "{}/{}/{nickname}.tracing.log",
        test_log_dir(),
        agent_gossip::mesh_prefix(mesh)
    );
    fs::read_to_string(path).unwrap_or_default()
}

/// Wait until `count_fn` returns >= `target` or `timeout` elapses.
pub fn wait_until(count_fn: impl Fn() -> usize, target: usize, timeout: Duration) -> usize {
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

/// Broadcast a mesh chat message via the CLI — `agent-gossip a2a call --method
/// SendMessage --text` with no `--to` (A2A is point-to-point, so a mesh-wide
/// message declares itself). Returns the raw `Output` with no success assertion,
/// so a caller testing a failure path can inspect it.
pub fn cli_message_raw(mesh: &str, nickname: &str, body: &str) -> Output {
    test_cmd()
        .args([
            "a2a",
            "broadcast",
            "--gossip",
            mesh,
            "--nickname",
            nickname,
            "--text",
            body,
        ])
        .output()
        .expect("a2a broadcast command failed to spawn")
}

/// Spawn `agent-gossip a2a msg …` — chat addressed to one peer. Returns the
/// raw [`Output`] so a caller can assert on failure (a msg legitimately fails
/// until the recipient's card has replicated).
#[must_use]
pub fn cli_msg_raw(mesh: &str, nickname: &str, to: &str, body: &str) -> Output {
    test_cmd()
        .args([
            "a2a",
            "msg",
            "--gossip",
            mesh,
            "--nickname",
            nickname,
            "--to",
            to,
            "--text",
            body,
        ])
        .output()
        .expect("a2a msg command failed to spawn")
}

/// [`cli_msg_raw`] + assert success + trim stdout.
pub fn cli_msg_checked(mesh: &str, nickname: &str, to: &str, body: &str) -> String {
    let out = cli_msg_raw(mesh, nickname, to, body);
    assert!(
        out.status.success(),
        "a2a msg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// [`cli_message_raw`] + trim stdout. No success assertion.
pub fn cli_message(mesh: &str, nickname: &str, body: &str) -> String {
    let out = cli_message_raw(mesh, nickname, body);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// [`cli_message_raw`] + assert success + trim stdout.
pub fn cli_message_checked(mesh: &str, nickname: &str, body: &str) -> String {
    let out = cli_message_raw(mesh, nickname, body);
    assert!(
        out.status.success(),
        "a2a broadcast failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Spawn `agent-gossip poll …`, assert success,
/// return trimmed stdout.
pub fn cli_poll(mesh: &str, nickname: &str, after: Option<&str>) -> String {
    let mut args = vec!["poll", "--gossip", mesh, "--nickname", nickname];
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
pub fn cli_poll_long(mesh: &str, nickname: &str, after: Option<&str>) -> (String, Duration) {
    let mut args = vec!["poll", "--gossip", mesh, "--nickname", nickname, "--long"];
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
pub fn ipc_raw(mesh: &str, nickname: &str, line: &str) -> String {
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

/// Block until `target`'s card is present in `nickname`'s `meta` doc — the real
/// precondition for any **directed** A2A call, and the subprocess counterpart of
/// [`InProcNode::await_peer_card`].
///
/// Gossip linkage is *not* this signal. A directed frame is sealed to the
/// recipient's published X25519 key (`a2a::card::peer_seal_key`) and is never
/// sent in plaintext, so the daemon rejects the call outright until that key
/// arrives. The key rides the `meta` document, which replicates on its own
/// schedule — independently of the broadcast path a linkage probe observes. A
/// call issued on linkage alone therefore races card propagation and fails with
/// "its encryption key is not known yet (cards still propagating)".
pub fn await_peer_card_cli(mesh: &str, nickname: &str, target: &str) {
    let pointer = format!("/peers/{target}/card");
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let raw = cli_channel_get(Channel::Meta, mesh, nickname);
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
            && value["document"].pointer(&pointer).is_some()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{target}'s card never reached {nickname}'s meta doc"
        );
        std::thread::sleep(POLL);
    }
}

/// Create a task on `to` via the CLI (`agent-gossip a2a call --to <peer> --method
/// SendMessage --text`) and return the raw `Output` (no success assertion —
/// callers that test the unknown-peer failure path inspect it).
///
/// Deliberately **unguarded**: it is the helper the unknown-peer and
/// unicast-retry tests use, and waiting on a card that will never arrive (or
/// that the test means to race) is exactly what they must not do. Callers
/// targeting a known peer want [`cli_task_create`].
pub fn cli_task_create_raw(mesh: &str, nickname: &str, to: &str, text: &str) -> Output {
    test_cmd()
        .args([
            "a2a",
            "call",
            "--gossip",
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

/// Create a task on a **known** peer and return the worker-minted task id
/// (parsed from the JSON-RPC response the call prints on stdout). Panics on
/// failure.
///
/// Waits for `to`'s card first: a directed call is rejected until the seal key
/// replicates, so issuing one the moment the mesh links is a race. See
/// [`await_peer_card_cli`].
pub fn cli_task_create(mesh: &str, nickname: &str, to: &str, text: &str) -> String {
    await_peer_card_cli(mesh, nickname, to);
    let out = cli_task_create_raw(mesh, nickname, to, text);
    // stdout, not just stderr: a rejected JSON-RPC call reports itself in the
    // response body it prints on stdout, so a stderr-only message renders as a
    // bare "a2a create failed:" — which is what CI failures looked like.
    assert!(
        out.status.success(),
        "a2a create failed ({}) from <{nickname}> to <{to}>\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("a2a call prints a JSON-RPC response");
    parsed["result"]["task"]["id"]
        .as_str()
        .expect("the worker returned a Task id")
        .to_string()
}

/// Who is following up, into which task, on whom — grouped so
/// [`cli_task_followup`] stays inside the argument budget.
#[derive(Debug)]
pub struct FollowupParams<'a> {
    pub mesh: &'a str,
    pub nickname: &'a str,
    pub to: &'a str,
    pub task_id: &'a str,
    pub text: &'a str,
}

/// Send a **follow-up into an existing task** via the CLI — an answer, an
/// approval, a change request. The `--task-id` is what makes it a follow-up: A2A
/// reads an absent `taskId` as "no task yet", so the same call without it opens a
/// second, unrelated task on the worker. Returns the parsed JSON-RPC response,
/// whose `result.task.id` must be the id passed in.
pub fn cli_task_followup(params: &FollowupParams<'_>) -> serde_json::Value {
    let &FollowupParams {
        mesh,
        nickname,
        to,
        task_id,
        text,
    } = params;
    let out = test_cmd()
        .args([
            "a2a",
            "call",
            "--gossip",
            mesh,
            "--nickname",
            nickname,
            "--to",
            to,
            "--method",
            "SendMessage",
            "--task-id",
            task_id,
            "--text",
            text,
        ])
        .output()
        .expect("a2a call command failed to spawn");
    assert!(
        out.status.success(),
        "a2a follow-up failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).expect("a2a call prints a JSON-RPC response")
}

/// Worker-emit a task artifact (the result) via the CLI. Panics on failure.
pub fn cli_task_artifact(mesh: &str, nickname: &str, task_id: &str, text: &str) {
    let out = test_cmd()
        .args([
            "a2a",
            "artifact",
            "--gossip",
            mesh,
            "--nickname",
            nickname,
            "--task-id",
            task_id,
            "--text",
            text,
        ])
        .output()
        .expect("a2a artifact command failed to spawn");
    assert!(
        out.status.success(),
        "a2a artifact failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Worker-emit a task status via the CLI (`agent-gossip a2a status`). Panics on failure.
pub fn cli_task_status(mesh: &str, nickname: &str, task_id: &str, state: &str) {
    let out = test_cmd()
        .args([
            "a2a",
            "status",
            "--gossip",
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

/// Spawn `agent-gossip peers …`, assert success, return trimmed stdout (the
/// raw `{ok, peers, peer_count}` JSON line).
pub fn cli_peers(mesh: &str, nickname: &str) -> String {
    let out = test_cmd()
        .args(["peers", "--gossip", mesh, "--nickname", nickname])
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
pub fn cli_ping(mesh: &str, nickname: &str) {
    let out = test_cmd()
        .args(["ping", "--gossip", mesh, "--nickname", nickname])
        .output()
        .expect("ping command failed to spawn");
    assert!(
        out.status.success(),
        "ping failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The CLI subcommand for a channel (`state` / `meta`) — `Channel::label` is
/// `pub(crate)`, not reachable from outside `agent-gossip`.
pub fn channel_subcommand(channel: Channel) -> &'static str {
    match channel {
        Channel::State => "state",
        Channel::Meta => "meta",
    }
}

/// Spawn `agent-gossip <channel> get … `, assert success, return trimmed stdout (the
/// raw `{ok, document}` JSON line). Drives the real CLI → IPC socket → daemon
/// read path the in-process harness bypasses.
pub fn cli_channel_get(channel: Channel, mesh: &str, nickname: &str) -> String {
    let out = test_cmd()
        .args([channel_subcommand(channel), "get"])
        .args(["--gossip", mesh, "--nickname", nickname])
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
pub fn cli_channel_merge(channel: Channel, mesh: &str, nickname: &str, merge: &str) -> Output {
    test_cmd()
        .args([channel_subcommand(channel), "merge"])
        .args(["--gossip", mesh, "--nickname", nickname, "--merge", merge])
        .output()
        .expect("channel merge failed to spawn")
}

// ── In-process harness (api::MeshSession) ──────────────────────

use agent_gossip::api::{A2aCallParams, CreateConfig, JoinConfig, MeshSession, TaskArtifactParams};
use agent_gossip::{
    Channel, MeshName, Message, MessageBody, MessageId, MessageKind, Nickname, OutputEvent,
    PresenceSubtype, TaskId, TaskState,
};
use tokio::sync::mpsc::UnboundedReceiver;

/// One in-process mesh node: a real [`MeshSession`] (real iroh
/// endpoint + the real `daemon::run` loop on a background task) plus
/// its captured [`OutputEvent`] stream. Drop-in analogue of the
/// subprocess [`Node`], but everything runs in the test process — so
/// coverage is recorded and teardown is deterministic.
pub struct InProcNode {
    pub session: MeshSession,
    rx: UnboundedReceiver<OutputEvent>,
    drained: Vec<OutputEvent>,
    pub mesh: String,
    pub nickname: String,
}

// `MeshSession` and the event receiver are not `Debug`; the identity plus how
// much has been captured is all a failing assertion needs anyway.
impl std::fmt::Debug for InProcNode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InProcNode")
            .field("mesh", &self.mesh)
            .field("nickname", &self.nickname)
            .field("drained", &self.drained.len())
            .finish_non_exhaustive()
    }
}

/// Validate a `&str` into a [`MeshName`] for the in-process harness.
fn test_mesh_name(name: &str) -> MeshName {
    MeshName::new(name).expect("valid test mesh name")
}

/// Route the daemon's `tracing` to the test's captured stdout when `RUST_LOG`
/// is set, so an in-process failure can be debugged the same way a subprocess
/// one can (`RUST_LOG=agent_habilis_mesh::gossip=debug cargo test …`). Without this
/// the in-process tests install no subscriber at all and every diagnostic the
/// engine emits is discarded — the reason a flake here had to be chased by
/// bisecting asserts rather than reading a log.
///
/// Idempotent and silent when `RUST_LOG` is unset: `try_init` fails harmlessly
/// if a subscriber is already installed, and an empty filter emits nothing, so
/// a normal run is unaffected.
pub fn init_test_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}

/// Whether this host can send to the mDNS multicast groups at all — the
/// precondition for any test whose peers find each other over LAN multicast.
///
/// Deliberately asymmetric: a successful send does **not** prove discovery
/// works (nothing may be listening), but a failure on *both* families proves it
/// cannot, so this only ever gates out a host that is certainly incapable. A
/// VPN that captures the default route, or a runner image without an `IPv6`
/// route, makes every `send_to` fail with `EHOSTUNREACH` — and an mDNS test
/// then burns its whole budget proving only that the host has no multicast.
pub fn mdns_multicast_available() -> bool {
    fn can_send(bind: &str, group: &str) -> bool {
        std::net::UdpSocket::bind(bind).is_ok_and(|sock| sock.send_to(&[0_u8], group).is_ok())
    }
    can_send("0.0.0.0:0", "224.0.0.251:5353") || can_send("[::]:0", "[ff02::fb]:5353")
}

impl InProcNode {
    /// Create a new private mesh. `self.mesh` holds the id.
    pub async fn create(name: &str) -> Self {
        init_test_tracing();
        Self::from_session(
            MeshSession::create(CreateConfig::new(test_mesh_name(name)))
                .await
                .expect("in-process create failed"),
        )
    }

    /// Create a new private mesh with an explicit nickname.
    pub async fn create_with_nick(name: &str, nick: &str) -> Self {
        init_test_tracing();
        let mut cfg = CreateConfig::new(test_mesh_name(name));
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        Self::from_session(
            MeshSession::create(cfg)
                .await
                .expect("in-process create failed"),
        )
    }

    /// Create a new private, password-protected mesh.
    pub async fn create_with_password(name: &str, password: &str) -> Self {
        init_test_tracing();
        let mut cfg = CreateConfig::new(test_mesh_name(name));
        cfg.password = Some(password.to_owned());
        Self::from_session(
            MeshSession::create(cfg)
                .await
                .expect("in-process passworded create failed"),
        )
    }

    /// Create a mesh under an explicit active-view cap (a small cap forces a
    /// partial mesh, so directed traffic must reach non-neighbours).
    pub async fn create_with_peers(name: &str, nick: &str, max_peers: usize) -> Self {
        init_test_tracing();
        let mut cfg = CreateConfig::new(test_mesh_name(name));
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
        cfg.max_peers = max_peers;
        Self::from_session(
            MeshSession::create(cfg)
                .await
                .expect("in-process transport create failed"),
        )
    }

    /// Join `mesh` under an explicit active-view cap.
    pub async fn join_with_peers(mesh: &str, nick: &str, max_peers: usize) -> Self {
        init_test_tracing();
        let target = mesh.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nick).expect("valid test nickname"));
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

    /// Join `mesh` (a mesh id) with an explicit nickname.
    pub async fn join(mesh: &str, nickname: &str) -> Self {
        init_test_tracing();
        let target = mesh.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        Self::from_session(
            MeshSession::join(cfg)
                .await
                .expect("in-process join failed"),
        )
    }

    /// Join the public topic mesh derived from `string`, with an explicit
    /// nickname.
    pub async fn topic(string: &str, nickname: &str) -> Self {
        init_test_tracing();
        let mut cfg = agent_gossip::api::TopicConfig::new(string.to_owned());
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        Self::from_session(
            MeshSession::topic(cfg)
                .await
                .expect("in-process topic join failed"),
        )
    }

    /// Join a password-protected `mesh`, surfacing the join error (so a
    /// wrong-password test can assert on it).
    pub async fn try_join_with_password(
        mesh: &str,
        nickname: &str,
        password: &str,
    ) -> Result<Self, agent_gossip::api::JoinError> {
        let target = mesh.parse().expect("valid test join target");
        let mut cfg = JoinConfig::new(target);
        cfg.nickname = Some(Nickname::new(nickname).expect("valid test nickname"));
        cfg.password = Some(password.to_owned());
        Ok(Self::from_session(MeshSession::join(cfg).await?))
    }

    /// Broadcast a plain message to everyone; returns the new message id.
    pub async fn broadcast(&self, text: &str) -> MessageId {
        self.session
            .broadcast(MessageBody::new(text).expect("valid body"))
            .await
            .expect("in-process broadcast failed")
            .id
    }

    /// Send a msg to one peer; returns the new message id.
    pub async fn msg(&self, to: &str, text: &str) -> MessageId {
        self.try_msg(to, text)
            .await
            .expect("in-process msg failed")
            .id
    }

    /// [`Self::msg`] without the panic — a msg is sealed to the recipient, so
    /// it legitimately fails until that peer's card has replicated, and a test
    /// that races the card needs to see the error rather than die on it.
    ///
    /// # Errors
    /// Propagates the send failure (loop stopped, or the recipient's seal key
    /// is not known yet).
    pub async fn try_msg(&self, to: &str, text: &str) -> anyhow::Result<Message> {
        self.session
            .msg(
                Nickname::new(to).expect("valid test nickname"),
                MessageBody::new(text).expect("valid body"),
            )
            .await
    }

    /// Apply an RFC 7386 merge to the shared state. Panics if the merge is
    /// rejected (not a JSON object / loop stopped).
    pub async fn state_merge(&self, merge: serde_json::Value) {
        self.try_state_merge(merge)
            .await
            .expect("in-process state_merge failed");
    }

    /// Like [`Self::state_merge`] but returns the raw result.
    async fn try_state_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        self.session.state_merge(merge).await
    }

    /// The current derived shared-state document (the merge fold over the
    /// state log).
    pub async fn state_get(&self) -> serde_json::Value {
        self.session
            .state_get()
            .await
            .expect("in-process state_get failed")
    }

    /// Apply an RFC 7386 merge to the `meta` channel. Panics on rejection —
    /// use [`Self::try_meta_merge`] to exercise rejection deliberately.
    pub async fn meta_merge(&self, merge: serde_json::Value) {
        self.try_meta_merge(merge)
            .await
            .expect("in-process meta_merge failed");
    }

    /// Like [`Self::meta_merge`] but returns the raw result.
    pub async fn try_meta_merge(&self, merge: serde_json::Value) -> anyhow::Result<()> {
        self.session.meta_merge(merge).await
    }

    /// The current derived `meta`-channel document.
    pub async fn meta_get(&self) -> serde_json::Value {
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
    pub async fn merge(&self, channel: Channel, merge: serde_json::Value) {
        match channel {
            Channel::State => self.state_merge(merge).await,
            Channel::Meta => self.meta_merge(merge).await,
        }
    }

    /// The current derived document for `channel`.
    pub async fn get(&self, channel: Channel) -> serde_json::Value {
        match channel {
            Channel::State => self.state_get().await,
            Channel::Meta => self.meta_get().await,
        }
    }

    /// Captured changes on `channel` so far, each `(derived document, is_self)`.
    pub fn changes(&mut self, channel: Channel) -> Vec<(serde_json::Value, bool)> {
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
    pub async fn wait_change(
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
    pub async fn create_task(&self, target: &str, text: &str) -> serde_json::Value {
        // Task creation seals the request to `target`'s card
        // (`a2a::card::peer_seal_key`). The card propagates asynchronously and the
        // seal is one-shot, so wait for it first — otherwise, under the concurrent
        // suite's load, the request is silently dropped and the call hangs.
        self.await_peer_card(target).await;
        let msg =
            agent_gossip::a2a::gossip::send_message_payload(self.session.mesh_id(), None, text);
        self.a2a_call_retrying(target, "SendMessage", serde_json::json!({ "message": msg }))
            .await
    }

    /// Block until `target`'s card is present in this node's `meta` doc — the
    /// seal-key source for any directed A2A call, and the precondition
    /// [`Self::a2a_call`] cannot enforce itself (the unknown-peer tests call a
    /// nickname whose card will never arrive and must fail fast, not block).
    /// [`Self::create_task`] applies it for you; a bare `a2a_call` to a real
    /// peer must call this first or it races card propagation.
    pub async fn await_peer_card(&self, target: &str) {
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
    pub async fn task_message(
        &self,
        target: &str,
        task_id: &TaskId,
        text: &str,
    ) -> serde_json::Value {
        let msg = agent_gossip::a2a::gossip::send_message_payload(
            self.session.mesh_id(),
            Some(task_id),
            text,
        );
        self.a2a_call_retrying(target, "SendMessage", serde_json::json!({ "message": msg }))
            .await
    }

    /// A directed A2A call to `target`, sent **exactly once**; returns the
    /// parsed JSON-RPC response.
    ///
    /// Reach for this only to assert on single-shot behaviour itself. Anything
    /// that merely needs the call to *land* wants [`Self::a2a_call_retrying`]:
    /// a directed request dropped by a momentarily linkless overlay is never
    /// retransmitted, so a lone attempt just burns the caller's deadline.
    pub async fn a2a_call(
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
                timeout: MSG_TIMEOUT,
            })
            .await
            .expect("a2a_call transport")
    }

    /// A small directed A2A call, **re-issued while the peer never answers**.
    ///
    /// A directed request is broadcast into the gossip overlay, and `A2aReq` /
    /// `A2aResp` are unlogged — anti-entropy never heals them. So a request sent
    /// while the overlay happens to hold no live peer link is dropped *for good*
    /// and the caller simply blocks to its deadline. That window is real and
    /// routine right after a join: `linked_endpoints` is written only by
    /// `NeighborUp`/`NeighborDown`, so a fresh pair can link, deliver, and then
    /// watch the neighbor go down, leaving the daemon at "silent partition
    /// (meshed but zero peer links)" until the next heal tick re-bridges it 15s
    /// later. A single-shot call therefore races overlay convergence — this was
    /// the suite's long-standing ~50% flake in `a2a_rpc`, reproducible on one
    /// test with no concurrency at all.
    ///
    /// Retrying is the same remedy the subprocess unicast test already applies
    /// (`gossip_network.rs`, "retry until it lands"), and it is safe here: the
    /// task registry treats a duplicate leg as a no-op (`a2a/task.rs`). Each
    /// attempt gets a short deadline so a dropped frame is re-sent promptly
    /// rather than burning the budget waiting on a frame the overlay discarded —
    /// which is exactly why this must stay off the large-payload path, where a
    /// resend overflows the outbound queue instead. See [`Self::a2a_call`].
    ///
    /// Only a transport timeout is retried. Any genuine answer — including a
    /// semantic error such as "not permitted" or "unknown peer" — means
    /// the round trip completed, and is returned untouched.
    pub async fn a2a_call_retrying(
        &self,
        target: &str,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let peer = Nickname::new(target).expect("valid target");
        let deadline = Instant::now() + MSG_TIMEOUT;
        loop {
            let response = self
                .session
                .a2a_call(A2aCallParams {
                    peer: peer.clone(),
                    method: method.to_owned(),
                    params: params.clone(),
                    timeout: RPC_ATTEMPT,
                })
                .await
                .expect("a2a_call transport");
            if !is_rpc_timeout(&response) || Instant::now() >= deadline {
                return response;
            }
        }
    }

    /// Worker-emit a `TaskStatusUpdate` on a task we're serving.
    pub async fn task_status(&self, task_id: &TaskId, state: TaskState, note: Option<&str>) {
        self.session
            .task_status(task_id.clone(), state, note.map(str::to_owned))
            .await
            .expect("in-process task_status failed");
    }

    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task we're serving.
    pub async fn task_artifact(&self, task_id: &TaskId, text: &str) {
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
    /// so a test can read the minted ticket reference.
    pub async fn task_artifact_file(&self, task_id: &TaskId, path: &Path) -> Message {
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
    pub fn tasks(&mut self) -> Vec<(&Message, bool)> {
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
    pub fn saw_task_message(&mut self) -> bool {
        self.pump();
        self.drained
            .iter()
            .any(|event| matches!(event, OutputEvent::TaskMessage { .. }))
    }

    /// Wait until a worker-pushed task frame in `state` (from any author) is
    /// surfaced.
    pub async fn wait_task_state(&mut self, state: TaskState, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::Task { msg, .. }
                        if agent_gossip::a2a::gossip::frame_task_state(msg) == Some(state)
                )
            })
        })
        .await
    }

    /// Wait until a `message`-kind task event (an RPC `message/send` leg) is
    /// surfaced to us (the worker).
    pub async fn wait_task_message(&mut self, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events
                .iter()
                .any(|event| matches!(event, OutputEvent::TaskMessage { .. }))
        })
        .await
    }

    /// Clean shutdown (broadcasts `Left`).
    pub async fn leave(self) {
        let _ = self.session.leave().await;
    }

    /// Non-blocking: move every pending captured event into the buffer.
    fn pump(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.drained.push(event);
        }
    }

    /// Every captured event so far (drains pending first).
    pub fn events(&mut self) -> &[OutputEvent] {
        self.pump();
        &self.drained
    }

    /// Captured events rendered to the documented `--output json`
    /// wire format and parsed — byte-identical to what the subprocess
    /// emitted.
    pub fn json_events(&mut self) -> Vec<serde_json::Value> {
        self.pump();
        self.drained
            .iter()
            .filter_map(agent_gossip::event_json)
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect()
    }

    /// Only `{"event":"message"}` lines (both `msg` and `presence`
    /// carry `event:"message"`).
    pub fn message_events(&mut self) -> Vec<serde_json::Value> {
        self.json_events()
            .into_iter()
            .filter(|value| value["event"] == "message")
            .collect()
    }

    /// `{"event":"message","type":"broadcast"}` lines only — gossip-wide chat,
    /// excluding presence and msgs.
    pub fn broadcast_events(&mut self) -> Vec<serde_json::Value> {
        self.chat_events("broadcast")
    }

    /// `{"event":"message","type":"msg"}` lines only — chat addressed to one
    /// peer.
    pub fn msg_events(&mut self) -> Vec<serde_json::Value> {
        self.chat_events("msg")
    }

    fn chat_events(&mut self, ty: &str) -> Vec<serde_json::Value> {
        self.json_events()
            .into_iter()
            .filter(|value| value["event"] == "message" && value["type"] == ty)
            .collect()
    }

    /// Inbound `msg`-kind messages captured so far (includes self
    /// echoes — filter on `is_self` if needed).
    pub fn messages(&mut self) -> Vec<&Message> {
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
    /// The subprocess [`Node`] parsed only peer lines, so ports that
    /// counted "deliveries" use this.
    pub fn inbound(&mut self) -> Vec<&Message> {
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
    pub async fn wait_body(&mut self, body: &str, timeout: Duration) -> bool {
        self.wait_for(timeout, |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    OutputEvent::Message { msg, is_self: false }
                        if agent_gossip::a2a::gossip::chat_text(msg).as_deref() == Some(body)
                )
            })
        })
        .await
    }

    /// Wait until at least `min_count` peer (non-self) messages have
    /// been captured, or `timeout` elapses.
    pub async fn wait_inbound(&mut self, min_count: usize, timeout: Duration) -> bool {
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
    pub fn count_body(&mut self, body: &str) -> usize {
        self.pump();
        self.drained
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    OutputEvent::Message { msg, is_self: false }
                        if agent_gossip::a2a::gossip::chat_text(msg).as_deref() == Some(body)
                )
            })
            .count()
    }

    /// Number of surfaced presence events of the given kind
    /// (`joined` when `joined` is true, else `left`).
    pub fn presence_count(&mut self, joined: bool) -> usize {
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
    pub async fn wait_presence(&mut self, nick: &str, joined: bool, timeout: Duration) -> bool {
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
    pub async fn wait_presence_count(
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
    pub async fn wait_for(
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
    pub async fn wait_messages(&mut self, min_count: usize, timeout: Duration) -> bool {
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

/// Wait until `left` and `right` each hold the *other's* card — the true
/// precondition for a directed A2A **round trip**, and the one a single
/// [`InProcNode::await_peer_card`] does not establish.
///
/// Both legs are sealed, so both directions must have converged. The caller
/// seals the request to the callee's key; the callee then seals its reply back
/// to the *caller's* key — and a responder that lacks the caller's card does not
/// error, it **drops the reply on the floor** (`a2a/node.rs`: "cannot seal a2a
/// rpc response — dropping"). The caller has no way to distinguish that from a
/// slow peer, so it blocks for the full RPC timeout and the test fails ~60s
/// later with "peer unreachable or slow".
///
/// The two directions are not equally likely to be ready, which is why guarding
/// only one hides the bug rather than fixing it: the creator publishes its card
/// when it mints the gossip, so a joiner tends to pick it up during its initial
/// `meta` sync, while the joiner's own card has to propagate *back* afterwards.
/// Waiting on the caller's view alone therefore passes most of the time and
/// flakes under load.
pub async fn await_mutual_cards(left: &InProcNode, right: &InProcNode) {
    left.await_peer_card(&right.nickname).await;
    right.await_peer_card(&left.nickname).await;
}

/// In-process analogue of the subprocess `three_peers`: a creator
/// plus two joiners (`mon-<suffix>-a` / `-b`), all meshed in this
/// process. The mesh id is `creator.mesh`.
pub async fn three_peers(suffix: &str) -> (InProcNode, InProcNode, InProcNode) {
    let creator = InProcNode::create(&format!("mon{suffix}")).await;
    let joiner_a = InProcNode::join(&creator.mesh, &format!("mon-{suffix}-a")).await;
    let joiner_b = InProcNode::join(&creator.mesh, &format!("mon-{suffix}-b")).await;
    (creator, joiner_a, joiner_b)
}

// ── Subprocess harness (real `agent-gossip` processes) ─────
//
// For the reliability / contract tests that must exercise the shipped
// binary: real SIGKILL / SIGSTOP-SIGCONT, real stdout, real
// Unix-socket IPC, real heal/anti-entropy timing.

/// Scan a `create`/`join` log's full contents so far for the mesh id and
/// nickname, filling in whichever of `mesh_id`/`nickname` is still unset. The
/// daemon's JSON `ready` event carries both (`gossip` / `nickname`).
fn scan_create_log(content: &str, mesh_id: &mut Option<String>, nickname: &mut Option<String>) {
    for line in content.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value["event"] != "ready" {
            continue;
        }
        if mesh_id.is_none() {
            *mesh_id = value["gossip"].as_str().map(str::to_owned);
        }
        if nickname.is_none() {
            *nickname = value["nickname"].as_str().map(str::to_owned);
        }
    }
}

#[derive(Debug)]
pub struct Node {
    child: Child,
    log: PathBuf,
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
    /// `create` CLI args (e.g. `["--public"]`).
    pub fn create_args(name: &str, extra: &[&str], flags: &[(&str, &str)]) -> (Self, String) {
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

    pub fn messages(&self) -> Vec<Msg> {
        parse_messages(&self.log_contents())
    }

    /// How many received messages match `(author, body)` exactly.
    pub fn count_from(&self, author: &str, body: &str) -> usize {
        self.messages()
            .iter()
            .filter(|msg| msg.author == author && msg.body == body)
            .count()
    }

    /// How many **distinct** messages from `author` have a body starting with
    /// `prefix` — the convergence metric for the anti-entropy gap-recovery
    /// tests (each sends `prefix-{i}` and waits for the full distinct set).
    pub fn count_distinct_from(&self, author: &str, prefix: &str) -> usize {
        self.messages()
            .iter()
            .filter(|msg| msg.author == author && msg.body.starts_with(prefix))
            .map(|msg| msg.body.clone())
            .collect::<std::collections::HashSet<_>>()
            .len()
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
    }

    /// SIGCONT — wake a [`stop`](Self::stop)ped process.
    pub fn cont(&self) {
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
pub fn chat_text(msg: &Message) -> String {
    agent_gossip::a2a::gossip::chat_text(msg).expect("a chat frame carries an a2a payload")
}

#[derive(Debug, Clone)]
pub struct Msg {
    pub author: String,
    pub body: String,
    /// The recipient on a `type:"msg"`; `None` on a `type:"broadcast"`.
    pub to: Option<String>,
}

/// Parse the daemon's JSON `message` events — **both** chat types, skipping
/// `presence`. Broadcast and msg are both chat and both carry `author`/`body`;
/// `Msg::to` is what separates them, so a test that cares asserts on it rather
/// than on which of the two arrived.
fn parse_messages(output: &str) -> Vec<Msg> {
    let mut msgs = Vec::new();
    for line in output.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value["event"] != "message" || (value["type"] != "broadcast" && value["type"] != "msg") {
            continue;
        }
        let (Some(author), Some(body)) = (value["author"].as_str(), value["body"].as_str()) else {
            continue;
        };
        if !body.is_empty() {
            msgs.push(Msg {
                author: author.to_string(),
                body: body.to_string(),
                to: value["to"].as_str().map(str::to_owned),
            });
        }
    }
    msgs
}

/// [`wait_until`] with the standard message-delivery timeout.
pub fn wait_total(total_fn: impl Fn() -> usize, target: usize) -> usize {
    wait_until(total_fn, target, MSG_TIMEOUT)
}
