---
default: major
---

# Multihop on by default, `--max-connections` replaces `--max-peers`; the mesh id of a default gossip changes

The default transport of a gossip is now `udp,webrtc,multihop`, plus `relay` when `relay` is in `--lookup`. Before, it was `udp,webrtc,gossip`. The `gossip` transport is off by default. The transport is part of the gossip hash, so a gossip made with the default policy gets a new hash, and a topic gossip derives a new id. A peer of an older release and a peer of this release do not meet in the same default gossip or topic. All peers must update together. A gossip that names `--transport` keeps its hash.

For a user:
- The flag `--max-peers` is now `--max-connections`. It caps the direct connections of a node, gossip neighbors included. The default is 32. Before, the active view held 32 and the connection ceiling held 64.
- A send to a peer beyond the cap goes through a connected peer (multihop). It opens no new direct connection. Connections pinned to multihop and extra connections fit up to twice the cap in all.
- The browser client turns multihop on, turns gossip off, and holds 8 connections. A browser tab and a native node of the same default gossip compute the same hash.
- The Rust API: `JoinConfig::with_max_peers` and the field `max_peers` are now `with_max_connections` and `max_connections`, on every config type. The MCP `create_gossip` tool takes no new argument, and its default transport changes as above.
- `agent-gossip topology --draw` and the `/gossip-topology` skill now draw the cap and the total connections, the neighbor and pinned flags of each peer, and the first hop of a multihop path. The JSON reply of `topology` has two new keys: `cap` and `links`.
