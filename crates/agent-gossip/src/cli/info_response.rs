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

/// Ask every daemon socket on this machine to identify itself.
///
/// Best-effort by design: a stale socket left by a `SIGKILL`ed daemon simply
/// fails to answer and is skipped, which is exactly the case this is here to
/// tell apart from a live one.
pub(super) async fn live_daemons() -> Vec<InfoResponse> {
    let mut daemons = Vec::new();
    for path in ipc::active_socket_paths(&crate::runtime_base()) {
        // `Info` carries no mesh — the daemon answers with its own identity.
        let Ok(response) = ipc::send_to_path(&path, &IpcCommand::Info).await else {
            continue;
        };
        let Ok(info) = serde_json::from_str::<InfoResponse>(&response) else {
            continue;
        };
        daemons.push(info);
    }
    daemons
}
