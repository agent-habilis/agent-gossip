use agent_gossip::runtime_base;

/// The IPC socket path of daemon `nickname` in `mesh`, the same path the daemon binds.
#[must_use]
pub fn socket_path(mesh: &str, nickname: &str) -> String {
    runtime_base()
        .join(agent_gossip::mesh_prefix(mesh))
        .join(format!("{nickname}.ipc.sock"))
        .display()
        .to_string()
}
