use anyhow::Result;
use habilis_network::protocol::{MeshId, Nickname};

use super::args::{PollIdentity, PollOpts};
use super::ready::wait_for_ready;
use crate::a2a::ipc::IpcCommand;

/// Query the running daemon's live peer roster. Always emits the
/// raw IPC JSON (`{ok, peers, peer_count}`), like `poll`.
pub(super) async fn poll(mut opts: PollOpts) -> Result<()> {
    // First, so `--max-secs` counts from the process start like the harness
    // clock does, readiness wait included.
    let poll_started = tokio::time::Instant::now();
    let identity = opts.identity()?;
    let PollOpts {
        after,
        long,
        settle_secs,
        max_secs,
        ..
    } = opts;
    // The state-file form waits for readiness first so a poll can be armed
    // before the daemon has minted its identity.
    let (mesh, nickname) = match identity {
        PollIdentity::StateFile(path) => {
            let ready =
                wait_for_ready(&path, habilis_network::runtime::tuning::READY_MAX.as_secs())
                    .await?;
            let carried = ready.identity();
            let missing = |field: &'static str| {
                anyhow::anyhow!("state file {} carries no {field}", path.display())
            };
            let mesh: MeshId = carried.mesh.ok_or_else(|| missing("gossip id"))?.parse()?;
            let nick: Nickname = carried
                .nickname
                .ok_or_else(|| missing("nickname"))?
                .parse()?;
            (mesh, nick)
        }
        PollIdentity::Session { gossip, nickname } => (gossip, nickname),
    };
    let bell_mesh = mesh.to_string();
    let mut bell_lock = None;
    let mut settle = settle_secs.map(std::time::Duration::from_secs);
    let max_duration = max_secs.map(std::time::Duration::from_secs);
    let cmd = IpcCommand::Poll { mesh, after, long };

    loop {
        // A duplicate bell takes the lock over once the first one exits, so
        // the flag does not go false while this one is still parked.
        if long && bell_lock.is_none() {
            bell_lock = super::bell::acquire(&bell_mesh, nickname.as_str()).await;
        }
        // After the lock, so the bell counts as armed while it settles. The
        // wait stops at `--max-secs`, or a long settle would outlast the bound.
        if let Some(wait) = settle.take() {
            let left = max_duration.map_or(wait, |max| {
                wait.min(max.saturating_sub(poll_started.elapsed()))
            });
            tokio::time::sleep(left).await;
        }
        // A quiet ring that ends the bell before a harness time limit stops
        // it: the same `[]` and exit 0 as any empty ring, so the agent re-arms
        // without a notice. Checked before each new request, never after a
        // response, so a request already parked still delivers its event.
        if max_duration.is_some_and(|max| poll_started.elapsed() >= max) {
            println!("[]");
            return Ok(());
        }
        let started = tokio::time::Instant::now();
        let resp = super::send::send(&cmd, &nickname).await?;
        // An empty response is the daemon closing the connection without
        // writing (shutdown mid-park) — `round_trip` maps that EOF to `""`.
        // Surface it instead of printing a blank line / retrying forever.
        anyhow::ensure!(
            !resp.is_empty(),
            "gossip daemon closed the connection without a response (shutting down?)"
        );
        // Clean daemon shutdown: end the poll with the truthful empty batch
        // and exit 0 — re-issuing would hit a socket that is about to vanish.
        // The sentinel is a control response and never reaches stdout. A
        // daemon that dies WITHOUT sending it (crash, SIGKILL) still errors.
        if resp == crate::a2a::surfaced::SHUTDOWN_SENTINEL {
            println!("[]");
            return Ok(());
        }
        if !(long && resp == "[]") {
            println!("{resp}");
            return Ok(());
        }
        // Both empty paths render exactly `[]`; an error response is a JSON
        // object, so it prints and exits above. Floor the cycle so a daemon
        // degrading long reads to immediate empties (waiter registry full)
        // can't spin this loop hot — the unchanged cursor re-reads anything
        // that lands during the sleep.
        let min_cycle = habilis_network::runtime::tuning::POLL_LONG_MIN_CYCLE;
        if let Some(remaining) = min_cycle.checked_sub(started.elapsed()) {
            tokio::time::sleep(remaining).await;
        }
    }
}
