/// URI that names the gossip binding in an agent card; peers match it byte for byte.
pub const GOSSIP_BINDING: &str = "https://agent-habilis.dev/a2a/binding/gossip/v1";
/// The extension URI a broadcast payload must carry.
///
/// Copied from the app crate's `a2a/extensions.rs`. Here rather than beside its one use in `driver.rs`, so
/// the whole duplicated set is in one file and the `agent-gossip-core`
/// extraction cannot miss it.
pub const EXT_MESH_BROADCAST: &str = "https://agent-habilis.dev/a2a/ext/mesh-broadcast/v1";
/// Extension URI for the shared state document in the gossip binding.
pub const EXT_MESH_STATE: &str = "https://agent-habilis.dev/a2a/ext/mesh-state/v1";
/// Extension URI for A2A JSON-RPC calls carried over the mesh.
pub const EXT_MESH_A2A_RPC: &str = "https://agent-habilis.dev/a2a/ext/mesh-a2a-rpc/v1";
/// Extension URI for the blob offload side-channel.
pub const EXT_MESH_BLOB: &str = "https://agent-habilis.dev/a2a/ext/mesh-blob/v1";
/// Extension URI for sealed (encrypted) payloads.
pub const EXT_MESH_SEAL: &str = "https://agent-habilis.dev/a2a/ext/mesh-seal/v1";

/// Metadata key set to `true` on a keepalive status; marks it as plumbing, not a state change.
pub const META_BEAT: &str = "mesh:beat";
/// Metadata key for finished units of work in a progress beat. Sent with `META_TOTAL`.
pub const META_DONE: &str = "mesh:done";
/// Metadata key for all units of work in a progress beat. Sent with `META_DONE`.
pub const META_TOTAL: &str = "mesh:total";
/// Metadata key for the free-text reason that goes with a status update.
pub const META_REASON: &str = "mesh:reason";
/// The initiator's one-line name for a task, so both parties label it
/// identically. Absent on a brief from a peer that does not set it.
pub const META_LABEL: &str = "mesh:label";
/// A worker beat's answer to "is my agent reading": whether a bell is armed.
/// Absent on a beat from a peer that does not send it.
pub(super) const META_BELL: &str = "mesh:bell";
/// Seconds since the worker agent last polled. Sent with [`META_BELL`].
pub(super) const META_LAST_POLL: &str = "mesh:last_poll";
/// Why a working task waits on its worker's own user. Rides a `working`
/// status; the next status leg clears it.
pub(super) const META_WAITING: &str = "mesh:waiting";
/// When the worker sent a status leg, in milliseconds on its clock. Tells two
/// legs of one wire second apart. Absent on a leg from a peer that does not
/// send it, which then orders by the second alone.
pub(super) const META_ORDER_MS: &str = "mesh:order_ms";
