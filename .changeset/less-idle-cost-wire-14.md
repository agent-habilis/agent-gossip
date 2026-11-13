---
default: major
---

# Cut idle cost and wire bytes, prove the identity of a member

The daemon does less work when the mesh is calm and sends fewer bytes.

A member can prove its identity. The `Joined` frame carries a proof signed by the persisted member key, and a peer can ask for it. A calm mesh sends no extra bytes for it.

The member key is kept next to the state file, as `<state>.key` with mode 0600. A second daemon on one state path now fails at start, because two live daemons cannot be one member.

For a user:
- A nickname that another member holds with another key prints `nickname_held`. Join again without `--nickname`, or choose a different one: a rejoin with the same explicit `--nickname` keeps the conflict.
- The discover stream prints `events_lost` with a count when it drops events. List the gossips again.
- The frame version and the state model changed again in the same release: see the change file `follow-signed-slices-wire-18.md`.
