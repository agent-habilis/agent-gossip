---
default: major
---

# Relay and multihop on by default, `--max-connections` replaces `--max-peers`; the mesh id of a default gossip changes

The default lookup of `create` is now `mdns,dht,relay`, and the default transport is `udp,webrtc,multihop`, plus `relay` when `relay` is in `--lookup`. Before, a `create` with no `--lookup` was loopback-only, and the transport was `udp,webrtc,gossip`. The `gossip` transport is off by default. The transport is part of the gossip hash, so a gossip made with the default policy gets a new hash, and a topic gossip derives a new id. A peer of an older release and a peer of this release do not meet in the same default gossip or topic. All peers must update together. The hash covers the lookups and the transport together, so a gossip keeps its hash only if you name both `--lookup` and `--transport`. Naming `--transport` alone is not enough, because the default lookup changed too.

For a user:
- A `create` with no `--lookup` (CLI and MCP `create_gossip`) now reaches other machines. The new value `--lookup loopback` keeps a gossip on this machine. It stands alone in the list. The Rust API keeps its loopback default in `CreateConfig`: a caller names the lookups it wants.
- The flag `--max-peers` is now `--max-connections`. It caps the direct connections of a node, gossip neighbors included. The default is 16. Before, the active view held 32 and the connection ceiling held 64.
- A send to a peer beyond the cap goes through a connected peer (multihop) when the transport policy names `multihop` and a route to the peer exists. It opens no new direct connection. Connections pinned to multihop fit up to twice the cap in all.
- If the policy has no multihop (for example `--transport udp,webrtc`), or no route exists, the send dials directly as before. It evicts an idle peer that is not a gossip neighbor. If no peer can go, the new connection is an extra: it counts toward twice the cap, and it closes after 60 s with no use.
- The browser client turns multihop on, turns gossip off, and holds 8 connections. A browser tab and a native node of the same default gossip compute the same hash.
- The Rust API: `JoinConfig::with_max_peers` and the field `max_peers` are now `with_max_connections` and `max_connections`, on every config type. The MCP `create_gossip` tool takes no new argument, and its default transport changes as above.
- `agent-gossip topology --draw` and the `/gossip-topology` skill now draw the cap and the total connections, the neighbor, pinned and extra flags of each peer, and the first hop of a multihop path. The JSON reply of `topology` has three new top-level keys: `cap`, `links` and `endpoints`.
