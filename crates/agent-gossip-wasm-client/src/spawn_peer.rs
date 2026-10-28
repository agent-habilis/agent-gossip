use std::cell::RefCell;
use std::sync::Arc;

use habilis_network::embed::SilentSink;
use habilis_network::net::TransportOpts;
use habilis_network::runtime::{Node, Resolved, SetupParams, SignalHandling, setup_mesh};
use wasm_bindgen::JsValue;

use crate::err::err;
use crate::gossip_driver::{GossipDriver, Inbox};
use crate::gossip_peer::GossipPeer;
use crate::max_direct_peers::MAX_DIRECT_PEERS;

pub(crate) async fn spawn_peer(
    resolved: Resolved,
    transports: TransportOpts,
) -> Result<GossipPeer, JsValue> {
    let Resolved { kind, author, .. } = resolved;
    let inbox = Inbox::new(author.as_str().to_owned());
    // A tab writes no files and binds no socket. Both are already optional on
    // the engine, so the defaults of `SetupParams::new` are the configuration.
    let setup_params = SetupParams::new(author.clone(), Arc::new(SilentSink::default()))
        .with_max_peers(MAX_DIRECT_PEERS)
        .with_max_direct(MAX_DIRECT_PEERS)
        .with_transports(transports);
    let config = setup_mesh(kind, setup_params)
        .await
        .map_err(|error| err("set up gossip", &error))?;

    let mesh_id = config.mesh_id().as_str().to_owned();
    let driver = GossipDriver::new(inbox.clone());
    // `SignalHandling::Leave` — there are no process signals in a tab, and the
    // engine's registration is host-only anyway.
    let node = Node::spawn(config, driver, None, SignalHandling::Leave);
    let name = node.name().to_string();

    Ok(GossipPeer {
        mesh_id,
        name,
        nickname: author.to_string(),
        inbox,
        node: RefCell::new(Some(node)),
    })
}
