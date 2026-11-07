---
default: major
---

# Cut idle cost and wire bytes, bound the shared state, prove the identity of a member; frame version is now 14.0

The daemon does less work when the mesh is calm and sends fewer bytes. The meta document resets to a new epoch when dead peers fill it, so its memory stays bounded. Digest window ids are base64.

The shared state document is bounded too. Every write carries a wall clock time in milliseconds, kept as a hybrid clock. A node cuts the state into a new epoch from a seed when it grows. A write that would pass the size limit fails with a clear error and a way out: delete keys with a null merge. A new member waits for its first sync answer, up to 30 seconds, before it can write state.

A member can prove its identity. The `Joined` frame carries a proof signed by the persisted member key, and a peer can ask for it. A calm mesh sends no extra bytes for it.

A peer on frame version 13.0 cannot read frames of version 14.0. Every consumer of the engine must move to frame version 14.0; all peers of a mesh must update together.

For a user, a refused state write names its reason and a way out.
The longest value that a write can carry is about 1.3 KB, and about 0.9 KB with a password.
A joiner takes a far epoch only as the answer to its first request.
A nickname that another member holds with another key prints `nickname_held`. Leave the gossip and join again to get a new nickname.
The discover stream prints `events_lost` with a count when it drops events. List the gossips again.
