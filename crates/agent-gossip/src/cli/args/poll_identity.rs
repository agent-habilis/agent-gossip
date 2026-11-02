use habilis_network::protocol::{MeshId, Nickname};

pub(crate) enum PollIdentity {
    Session { gossip: MeshId, nickname: Nickname },
    StateFile(std::path::PathBuf),
}
