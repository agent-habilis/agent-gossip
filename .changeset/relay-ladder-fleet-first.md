---
default: major
---

# The default relay ladder puts the relays of the fleet first

The default relay ladder now has 7 rungs: 3 relays of the fleet, then 4 n0 relays. Rung 0 is a direct relay. Rungs 1 and 2 are tunnels. Before, the ladder had only the n0 relays.

For a user:
- A gossip on the default relay choice homes on the relays of the fleet first. A peer of an older release homes on a different rung. All peers must update together.
- Only rung 0 answers QUIC address discovery. A peer that is homed on a tunnel rung learns no public address from its relay.
