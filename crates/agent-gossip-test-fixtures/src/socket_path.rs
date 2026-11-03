use agent_gossip::runtime_base;

/// The file that marks the IPC endpoint of daemon `nickname` in `mesh`.
///
/// On Unix it is the socket, the same path the daemon binds; on Windows it is
/// the marker that holds the pipe name. It exists once the node is bound, so a
/// test waits on it.
#[must_use]
pub fn socket_path(mesh: &str, nickname: &str) -> String {
    #[cfg(unix)]
    let file = format!("{nickname}.ipc.sock");
    #[cfg(windows)]
    let file = format!("{nickname}.ipc");
    runtime_base()
        .join(agent_gossip::mesh_prefix(mesh))
        .join(file)
        .display()
        .to_string()
}
