use std::cell::RefCell;

use habilis_network::protocol::{
    DirectorySelection, JoinTarget, LookupOpts, MeshConfig, MeshName, TransportPolicy,
};
use habilis_network::runtime::{CreateParams, JoinParams, Node};
use wasm_bindgen::prelude::{JsValue, wasm_bindgen};

use crate::driver::{GossipDriver, Inbox, Request};
use crate::err::err;
use crate::parse_nickname::parse_nickname;
use crate::parse_path_mode::parse_path_mode;
use crate::spawn_peer::spawn_peer;

/// One membership of one mesh.
#[wasm_bindgen]
#[derive(Debug)]
pub struct GossipPeer {
    pub(crate) mesh_id: String,
    pub(crate) name: String,
    pub(crate) nickname: String,
    pub(crate) inbox: Inbox,
    /// `RefCell` because wasm-bindgen cannot hand out `self` by value — see the
    /// note on [`GossipPeer::leave`].
    pub(crate) node: RefCell<Option<Node<GossipDriver>>>,
}

#[wasm_bindgen]
impl GossipPeer {
    /// Mint a new mesh and join it. The returned peer's `mesh_id` is what
    /// another peer — browser or CLI — passes to [`GossipPeer::join`].
    ///
    /// # Errors
    /// Endpoint bind failure, or no reachable relay.
    pub async fn create(
        nickname: Option<String>,
        path_mode: Option<String>,
    ) -> Result<GossipPeer, JsValue> {
        console_error_panic_hook::set_once();
        let transports = parse_path_mode(path_mode.as_deref())?;
        let resolved = CreateParams {
            name: MeshName::random(),
            nickname: parse_nickname(nickname)?,
            // The relay ladder is the load-bearing leg: it is the only one a tab
            // has, and the rendezvous homes on it.
            config: MeshConfig {
                lookups: LookupOpts::public_preset(),
                password: None,
                issuer_pubkey: None,
                transport: TransportPolicy {
                    udp: true,
                    webrtc: true,
                    multihop: false,
                    gossip: true,
                    relay_transport: true,
                },
            },
            advertise: DirectorySelection::Unset,
            password: None,
            invite_only: false,
        }
        .resolve()
        .map_err(|error| err("resolve create params", &error))?;
        spawn_peer(resolved, transports).await
    }

    /// Join an existing mesh by its id.
    ///
    /// # Errors
    /// Unparseable id, endpoint bind failure, or no reachable relay.
    pub async fn join(
        mesh_id: String,
        nickname: Option<String>,
        path_mode: Option<String>,
    ) -> Result<GossipPeer, JsValue> {
        console_error_panic_hook::set_once();
        let transports = parse_path_mode(path_mode.as_deref())?;
        let target = mesh_id
            .trim()
            .parse::<JoinTarget>()
            .map_err(|error| err("parse gossip id", &error))?;
        let resolved = JoinParams {
            target,
            nickname: parse_nickname(nickname)?,
            password: None,
        }
        .resolve()
        .map_err(|error| err("resolve join params", &error))?;
        spawn_peer(resolved, transports).await
    }

    #[wasm_bindgen(getter, js_name = meshId)]
    #[must_use]
    pub fn mesh_id(&self) -> String {
        self.mesh_id.clone()
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn nickname(&self) -> String {
        self.nickname.clone()
    }

    /// The roster as a JSON array of `{ nickname }`, self excluded — a tab is
    /// never on its own roster, and listing itself would double-count.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn peers(&self) -> String {
        self.inbox.peers_json()
    }

    /// Messages with `seq` greater than `after`, as a JSON array.
    ///
    /// Polled rather than pushed: a callback into JS from the event loop would
    /// have to cross the wasm boundary on the receive path, and the page already
    /// samples on a timer for the roster.
    #[must_use]
    pub fn messages(&self, after: u32) -> String {
        self.inbox.messages_json(after)
    }

    /// Broadcast to everyone on the mesh.
    ///
    /// # Errors
    /// The event loop has stopped.
    pub async fn broadcast(&self, text: String) -> Result<(), JsValue> {
        self.request(Request::Broadcast(text)).await
    }

    async fn request(&self, req: Request) -> Result<(), JsValue> {
        // The sender is taken and the borrow dropped *before* the await. Holding
        // a `RefCell` borrow across one would panic on any re-entrant call from
        // JS, which is ordinary here: two clicks, or a send while polling.
        let sender = {
            let node = self.node.borrow();
            let node = node
                .as_ref()
                .ok_or_else(|| JsValue::from_str("peer has left"))?;
            node.sender()
        };
        sender
            .send(req)
            .await
            .map_err(|_| JsValue::from_str("the gossip event loop has stopped"))
    }

    /// Leave the mesh.
    ///
    /// Takes `&self`, never `self`: a `self`-by-value method compiles through
    /// wasm-bindgen to `__destroy_into_raw()`, so a double click would pass a
    /// null pointer and — under `panic = "abort"` — trap the whole instance.
    pub async fn leave(&self) -> Result<(), JsValue> {
        let node = self.node.borrow_mut().take();
        if let Some(node) = node {
            node.leave()
                .await
                .map_err(|error| err("leave gossip", &error))?;
        }
        Ok(())
    }
}
