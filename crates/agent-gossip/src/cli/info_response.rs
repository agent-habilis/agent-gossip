use habilis_network::runtime::ipc;

use crate::a2a::ipc_command::IpcCommand;

// The `room`/`participant_count` aliases mirror the state-file read fallback:
// a daemon from the previous release may still be serving, and doctor must
// list it rather than silently skip its unparseable info reply.
#[derive(serde::Deserialize)]
pub(super) struct InfoResponse {
    #[serde(alias = "room")]
    pub(super) gossip: String,
    pub(super) name: String,
    pub(super) nickname: String,
    #[serde(alias = "participant_count")]
    pub(super) peer_count: usize,
    /// `None` from a daemon that predates the field.
    pub(super) pid: Option<u32>,
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
