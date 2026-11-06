---
default: major
---

# Cut idle cost and wire bytes, bound the shared state, prove the identity of a member; frame version is now 14.0

The daemon does less work when the mesh is calm and sends fewer bytes. The meta document resets to a new epoch when dead peers fill it, so its memory stays bounded. Digest window ids are base64.

The shared state document is bounded too. Every write carries a Lamport time. A node cuts the state into a new epoch from a seed when it grows. A write that would pass the size limit fails with a clear error and a way out: delete keys with a null merge. A new member waits for its first sync answer, up to 30 seconds, before it can write state.

A member can prove its identity. The `Joined` frame carries a proof signed by the persisted member key, and a peer can ask for it. A calm mesh sends no extra bytes for it.

A peer on frame version 13.0 cannot read frames of version 14.0: all peers of a mesh must update together. `agent-share` and `mallorca` pin the same engine revision and must move to 14.0 with it.
