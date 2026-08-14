//! A gossip peer, in a browser tab.
//!
//! This is the same engine the CLI runs — `fofoca` with its `host` feature off
//! — so a tab is a first-class member rather than a client of one. It creates
//! or joins a mesh, appears on every other member's roster, and negotiates
//! direct WebRTC data channels with them.
//!
//! Discovery needs no lookup service, which is what makes this work in a tab at
//! all: the mesh id carries a seed, the seed derives a rendezvous identity, and
//! every peer pre-registers that identity at a known relay rung. A browser has
//! no mDNS and no DHT; it does not need them, and asking for them costs nothing
//! — a CLI peer on the same mesh still uses both.

mod driver;
mod wire;

use std::cell::RefCell;
use std::sync::Arc;

use fofoca::net::TransportOpts;
use fofoca::protocol::{
    DirectorySelection, JoinTarget, LookupOpts, MeshConfig, MeshName, Nickname,
};
use fofoca::runtime::{CreateParams, JoinParams, Node, Resolved, SetupParams, setup_mesh};
use fofoca::embed::SilentSink;
use wasm_bindgen::prelude::*;

use driver::{GossipDriver, Inbox, Request};

/// How many direct WebRTC sessions a tab will hold open.
const MAX_DIRECT_PEERS: usize = 8;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// One membership of one mesh.
#[wasm_bindgen]
pub struct GossipPeer {
    mesh_id: String,
    name: String,
    nickname: String,
    inbox: Inbox,
    /// `RefCell` because wasm-bindgen cannot hand out `self` by value — see the
    /// note on [`GossipPeer::leave`].
    node: RefCell<Option<Node<GossipDriver>>>,
}

#[wasm_bindgen]
impl GossipPeer {
    /// Mint a new mesh and join it. The returned peer's `mesh_id` is what
    /// another peer — browser or CLI — passes to [`GossipPeer::join`].
    ///
    /// # Errors
    /// Endpoint bind failure, or no reachable relay.
    pub async fn create(nickname: Option<String>, transport: Option<String>) -> Result<GossipPeer, JsValue> {
        console_error_panic_hook::set_once();
        let transports = parse_transport(transport.as_deref())?;
        let resolved = CreateParams {
            name: MeshName::random(),
            nickname: parse_nickname(nickname)?,
            // The relay ladder is the load-bearing leg: it is the only one a tab
            // has, and the rendezvous homes on it.
            config: MeshConfig {
                lookups: LookupOpts::public_preset(),
                password: None,
                issuer_pubkey: None,
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
        transport: Option<String>,
    ) -> Result<GossipPeer, JsValue> {
        console_error_panic_hook::set_once();
        let transports = parse_transport(transport.as_deref())?;
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
            let node = node.as_ref().ok_or_else(|| JsValue::from_str("peer has left"))?;
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

fn parse_nickname(nickname: Option<String>) -> Result<Option<Nickname>, JsValue> {
    match nickname.as_deref().map(str::trim).filter(|nick| !nick.is_empty()) {
        None => Ok(None),
        Some(nick) => nick
            .parse::<Nickname>()
            .map(Some)
            .map_err(|error| err("parse nickname", &error)),
    }
}

/// `dynamic` lets a failed ICE negotiation fall back to the relay; `webrtc`
/// fails loudly instead. Named rather than inferred so the caller states the
/// contract it is asserting.
fn parse_transport(mode: Option<&str>) -> Result<TransportOpts, JsValue> {
    match mode.map(str::trim).filter(|mode| !mode.is_empty()) {
        None | Some("dynamic") => Ok(TransportOpts::default()),
        Some("webrtc") => Ok(TransportOpts::webrtc_only()),
        Some(other) => Err(JsValue::from_str(&format!(
            "unknown transport {other:?}; expected `webrtc` or `dynamic`"
        ))),
    }
}

async fn spawn_peer(resolved: Resolved, transports: TransportOpts) -> Result<GossipPeer, JsValue> {
    let Resolved { kind, author, .. } = resolved;
    let inbox = Inbox::new(author.as_str().to_owned());
    let config = setup_mesh(
        kind,
        SetupParams {
            author: author.clone(),
            max_peers: MAX_DIRECT_PEERS,
            endpoint: None,
            protocols: Vec::new(),
            transports,
            // A tab writes no files and binds no socket. Both are already
            // optional on the engine, so this is configuration rather than a
            // special case.
            runtime_base: None,
            state_file: None,
            sink: Arc::new(SilentSink),
            multihop: false,
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

fn err(context: &str, error: &impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("{context}: {error}"))
}

