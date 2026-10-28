use agent_gossip::runtime_base;

pub fn socket_path(mesh: &str, nickname: &str) -> String {
    runtime_base()
        .join(agent_gossip::mesh_prefix(mesh))
        .join(format!("{nickname}.ipc.sock"))
        .display()
        .to_string()
}
