pub const GOSSIP_BINDING: &str = "https://agent-habilis.dev/a2a/binding/gossip/v1";
pub const EXT_MESH_BROADCAST: &str = "https://agent-habilis.dev/a2a/ext/mesh-broadcast/v1";
pub const EXT_MESH_STATE: &str = "https://agent-habilis.dev/a2a/ext/mesh-state/v1";
pub const EXT_MESH_A2A_RPC: &str = "https://agent-habilis.dev/a2a/ext/mesh-a2a-rpc/v1";
pub const EXT_MESH_BLOB: &str = "https://agent-habilis.dev/a2a/ext/mesh-blob/v1";
pub const EXT_MESH_SEAL: &str = "https://agent-habilis.dev/a2a/ext/mesh-seal/v1";

pub const META_BEAT: &str = "mesh:beat";
pub const META_DONE: &str = "mesh:done";
pub const META_TOTAL: &str = "mesh:total";
pub const META_REASON: &str = "mesh:reason";
/// The initiator's one-line name for a task, so both parties label it
/// identically. Absent on a brief from a peer that does not set it.
pub const META_LABEL: &str = "mesh:label";
