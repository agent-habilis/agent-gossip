//! A gossip peer, in a browser tab.
//!
//! This is the same engine the CLI runs — `habilis-network` without its
//! host-only code — so a tab is a first-class member rather than a client of
//! one. It creates or joins a mesh, appears on every other member's roster, and
//! negotiates direct WebRTC data channels with them.
//!
//! Discovery needs no lookup service, which is what makes this work in a tab at
//! all: the mesh id carries a seed, the seed derives a rendezvous identity, and
//! every peer pre-registers that identity at a known relay rung. A browser has
//! no mDNS and no DHT; it does not need them, and asking for them costs nothing
//! — a CLI peer on the same mesh still uses both.
//!
//! # Example
//!
//! ```js
//! import init, { GossipPeer } from "./agent_gossip_wasm_client.js";
//!
//! await init();
//! const peer = await GossipPeer.create(null, null);
//! ```

pub use self::gossip_peer::GossipPeer;
pub use self::start::start;

mod gossip_driver;
mod err;
mod gossip_peer;
mod max_direct_peers;
mod parse_nickname;
mod parse_path_mode;
mod spawn_peer;
mod start;
mod wire;
