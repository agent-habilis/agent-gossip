use std::path::Path;
use std::time::Duration;

use habilis_network::runtime::ipc;

use crate::a2a::ipc_command::IpcCommand;

#[derive(serde::Deserialize)]
pub(super) struct InfoResponse {
    pub(super) gossip: String,
    pub(super) name: String,
    pub(super) nickname: String,
    pub(super) peer_count: usize,
    pub(super) pid: u32,
}

/// A live daemon answers `Info` in milliseconds. A daemon whose event loop spins still completes the connection
/// and never answers, and with no limit that one daemon hangs `leave`, `session` and `doctor` of every session on
/// the host.
const PROBE_LIMIT: Duration = Duration::from_secs(2);

/// Ask every daemon socket on this machine to identify itself.
///
/// Best-effort by design: a stale socket left by a `SIGKILL`ed daemon simply
/// fails to answer and is skipped, which is exactly the case this is here to
/// tell apart from a live one.
pub(super) async fn live_daemons() -> Vec<InfoResponse> {
    let mut daemons = Vec::new();
    for path in ipc::active_socket_paths(&crate::runtime_base()) {
        if let Some(info) = probe(&path).await {
            daemons.push(info);
        }
    }
    daemons
}

async fn probe(path: &Path) -> Option<InfoResponse> {
    // `Info` carries no mesh — the daemon answers with its own identity.
    let answer = tokio::time::timeout(PROBE_LIMIT, ipc::send_to_path(path, &IpcCommand::Info));
    let response = answer.await.ok()?.ok()?;
    serde_json::from_str(&response).ok()
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    /// The socket of a daemon whose event loop spins: the kernel completes the connection from the listen
    /// queue, and no code ever reads the request.
    #[tokio::test(start_paused = true)]
    async fn a_daemon_that_accepts_and_never_answers_is_skipped() {
        let folder = std::env::temp_dir().join(format!("ag-probe-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("the temp folder is writable");
        let path = folder.join("mute.ipc.sock");
        let _listener = UnixListener::bind(&path).expect("the socket path is free and short");

        let outcome = tokio::time::timeout(Duration::from_secs(3_600), super::probe(&path)).await;

        let _ = std::fs::remove_dir_all(&folder);
        assert!(
            outcome.is_ok_and(|info| info.is_none()),
            "the probe waited 1 hour on a daemon that is mute, so `leave` hangs for every session on the host"
        );
    }
}
