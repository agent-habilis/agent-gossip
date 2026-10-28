use std::cell::RefCell;
use std::sync::Arc;

use habilis_network::embed::SilentSink;
use habilis_network::net::TransportOpts;
use habilis_network::runtime::{Node, Resolved, SetupParams, setup_mesh};
use wasm_bindgen::JsValue;

use crate::driver::{GossipDriver, Inbox};
use crate::err::err;
use crate::gossip_peer::GossipPeer;
use crate::max_direct_peers::MAX_DIRECT_PEERS;

pub(crate) async fn spawn_peer(
    resolved: Resolved,
    transports: TransportOpts,
) -> Result<GossipPeer, JsValue> {
    let Resolved { kind, author, .. } = resolved;
    let inbox = Inbox::new(author.as_str().to_owned());
    let config = setup_mesh(
        kind,
        SetupParams {
            author: author.clone(),
            max_peers: MAX_DIRECT_PEERS,
            max_direct: MAX_DIRECT_PEERS,
            endpoint: None,
            protocols: Vec::new(),
            transports,
            // A tab writes no files and binds no socket. Both are already
            // optional on the engine, so this is configuration rather than a
            // special case.
            runtime_base: None,
            state_file: None,
            sink: Arc::new(SilentSink),
            per_peer_gate: None,
            cohost: None,
            live_count: None,
        },
    )
    .await
    .map_err(|error| err("set up gossip", &error))?;

    let mesh_id = config.mesh_id().as_str().to_owned();
    let driver = GossipDriver::new(inbox.clone());
    // `handle_signals: false` — there are no process signals in a tab, and the
    // engine's registration is host-only anyway.
    let node = Node::spawn(config, driver, None, false);
    let name = node.name().to_string();

    Ok(GossipPeer {
        mesh_id,
        name,
        nickname: author.to_string(),
        inbox,
        node: RefCell::new(Some(node)),
    })
}
