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
/// Metadata key for the progress of a check-in or a beat: a whole percentage, 0 to 100.
pub const META_PROGRESS: &str = "mesh:progress";
/// Metadata key for the free-text reason that goes with a status update.
pub const META_REASON: &str = "mesh:reason";
/// The initiator's one-line name for a task, so both parties label it
/// identically. Absent on a brief from a peer that does not set it.
pub const META_LABEL: &str = "mesh:label";
/// Set to `true` on a check-in while the worker waits on its own user. The next status leg
/// clears it, and a leg that leaves it out says the worker does not wait.
pub(super) const META_WAITING_USER: &str = "mesh:waiting_user";
/// Set to `true` on the `Task` that answers an opening whose reply did not come before the
/// deadline: the task exists on this node, and nothing says that the worker got it yet.
pub(super) const META_UNCONFIRMED: &str = "mesh:unconfirmed";
/// The place of a leg in the sequence of its sender, per task: the initiator's opening is 1 and
/// each follow-up one more, and the worker numbers its status and artifact legs the same way. The
/// daemon stamps it. A beat of the worker carries the seq of its last leg and does not count; a
/// beat of the initiator has none. A leg whose seq is not above the highest that the receiver took
/// in is dropped whole.
pub(super) const META_SEQ: &str = "mesh:seq";
/// Seconds since the worker agent last checked in on a task, on a worker beat.
/// The sender takes the smaller of it and the silence it measured itself.
pub(super) const META_CHECKIN_AGE: &str = "mesh:checkin_age";
/// What changed, from the last check-in of a task, in `GetTask` and `ListTasks`.
pub(super) const META_STATUS_TEXT: &str = "mesh:status_text";
