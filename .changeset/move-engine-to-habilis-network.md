---
default: major
---

# Move the engine to habilis-network

agent-gossip now runs on habilis-network, the successor of the previous engine. This release is breaking.

Every gossip hash, mesh id and invite ticket made by an older version stops working, because mesh ids and tickets are now version 2. Create the gossip again, and invite your peers again. A member of an older version and a member of this version never share a gossip.

#### New defaults

A gossip now carries `udp,webrtc,gossip`. The gossip transport reaches a peer that has no direct path in the frames of the gossip itself. Multihop is off by default, because it starts a second endpoint on every native member.

The relay carries payload when `relay` is in `--lookup`. A plain `create` is still loopback, so it does not use the relay. `create --advertise` with no `--lookup` is now public: it takes `mdns,dht,relay`, because a directory listing needs a lookup that reaches other machines. A topic gossip carries `udp,webrtc,gossip,relay`.

#### `--transport` words

`--transport` takes a list of `udp`, `webrtc`, `multihop`, `gossip` and `relay`. The list is literal: it names exactly the paths the gossip carries, and it must name `udp` or `webrtc`. The old words `p2p` and `p2p,relay` are removed. The MCP tool `create_gossip` takes the same words in its `transport` array.

#### Removed

The hidden `--multihop` flag is removed. Multihop is now a part of the gossip id. Name it in `--transport` when you want it.

#### Known limit

Directory sessions still use the default policy of the engine, which has multihop on. This stays so until habilis-network lets the app set the policy of a directory session.
