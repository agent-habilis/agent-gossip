//! The safe Rust core behind the C ABI: an engine driver plus a blocking handle
//! a foreign caller can drive from one thread.
//!
//! **Twin file:** `examples/mesh-pipe/src/main.rs`. The frame taxonomy here
//! (`pipe_data` / `pipe_eof`, base64 body, the [`AppClass`] flags, the chunk
//! budget) is deliberately identical to that binary's, so the two interoperate
//! over one mesh. The duplication is the price of keeping `mesh-pipe` dependent
//! on nothing but the engine; `tests/c_suite.rs` pipes bytes in both directions
//! between them, so any divergence here fails there rather than silently
//! splitting the wire format.

use std::sync::Arc;
use std::time::Duration;

use agent_habilis_mesh::async_trait;
use agent_habilis_mesh::embed::EventLoopState;
use agent_habilis_mesh::embed::HandlerCtx;
use agent_habilis_mesh::embed::NodeDriver;
use agent_habilis_mesh::embed::SilentSink;
use agent_habilis_mesh::embed::{AppClass, InboundApp, NodeApp};
use agent_habilis_mesh::ops::{StateMergeParams, broadcast_state_merge, send_app};
use agent_habilis_mesh::protocol::JoinTarget;
use agent_habilis_mesh::protocol::{
    AppFrameParams, AppTag, Channel, Message, MessageBody, MessageKind, Nickname,
};
use agent_habilis_mesh::protocol::{
    DirectorySelection, LookupSet, MeshConfig, MeshName, RelaySelection, resolve_lookups,
};
use agent_habilis_mesh::runtime::{CreateParams, JoinParams, Node, Resolved, TopicParams};
use agent_habilis_mesh::runtime::{SetupKind, SetupParams, setup_mesh};
use agent_habilis_mesh::util::consts::{GOSSIP_ACTIVE_VIEW_CAPACITY, MAX_MESSAGE_SIZE};

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio::sync::{mpsc, oneshot};

/// The `App`-frame tags, byte-identical to `examples/mesh-pipe`'s — the engine
/// routes on the tag but never interprets it.
mod tag {
    pub(crate) const DATA: &str = "pipe_data";
    pub(crate) const EOF: &str = "pipe_eof";
}

/// Inbound frames buffered for a foreign caller that reads on its own schedule.
/// Bounded, because a handle that only ever *sends* still receives broadcasts:
/// an unbounded queue would grow for the process's lifetime.
const INBOUND_CAP: usize = 256;

/// One surfaced inbound frame, flattened for the C boundary.
#[derive(Debug)]
pub struct Inbound {
    /// The frame's author nickname.
    pub nick: String,
    /// The frame was addressed to us specifically, not broadcast.
    pub directed: bool,
    /// A `pipe_eof` marker; `bytes` is then empty.
    pub eof: bool,
    pub bytes: Vec<u8>,
}

/// A request pushed into the event loop from the caller's thread. Every arm
/// carries its own reply channel: a foreign caller needs the outcome of a send
/// as a return code, not a log line.
#[expect(
    missing_debug_implementations,
    reason = "the oneshot reply senders carry no Debug bound worth adding for a value that never reaches a log line"
)]
pub enum Request {
    Send {
        tag: AppTag,
        to: Option<Nickname>,
        body: MessageBody,
        reply: oneshot::Sender<Result<(), String>>,
    },
    StateMerge {
        merge: serde_json::Value,
        reply: oneshot::Sender<Result<(), String>>,
    },
    StateJson {
        reply: oneshot::Sender<String>,
    },
    Peers {
        reply: oneshot::Sender<String>,
    },
    PeerCount {
        reply: oneshot::Sender<usize>,
    },
}

/// The engine seam. Every inbound `pipe_*` frame is queued for the foreign
/// caller instead of being written anywhere — this crate owns no stdio.
#[derive(Debug)]
struct PipeApp {
    inbound: mpsc::Sender<Inbound>,
}

#[async_trait]
impl NodeApp for PipeApp {
    fn classify(&self, _message: &Message) -> AppClass {
        // Identical to mesh-pipe's: ephemeral stream bytes, never logged, never
        // a task beat, always valid (an opaque base64 body), no per-author hash
        // chain. `sealed: false` is load-bearing — this consumer publishes no
        // a2a card, so it can neither seal to a peer nor be sealed to, and the
        // addressee must pass a directed plaintext body straight through
        // instead of trying (and failing) to unseal it.
        AppClass {
            loggable: false,
            beat: false,
            valid: true,
            chained: false,
            sealed: false,
        }
    }

    async fn on_app_frame(
        &mut self,
        frame: InboundApp<'_>,
        _state: &mut EventLoopState,
        _ctx: &HandlerCtx<'_>,
    ) -> bool {
        let InboundApp {
            message,
            surfaceable: _,
        } = frame;
        // The engine only dispatches frames addressed to us or broadcast, so a
        // present `to` is us.
        let directed = matches!(message.kind, MessageKind::App { to: Some(_), .. });
        let queued = match message.kind.app_tag().map(AppTag::as_str) {
            Some(tag::DATA) => match BASE64.decode(message.body.as_str()) {
                Ok(bytes) => Some(Inbound {
                    nick: message.author.to_string(),
                    directed,
                    eof: false,
                    bytes,
                }),
                Err(error) => {
                    tracing::warn!(
                        target: "agent_habilis_mesh::messages",
                        %error,
                        "dropping undecodable pipe_data"
                    );
                    None
                }
            },
            Some(tag::EOF) => Some(Inbound {
                nick: message.author.to_string(),
                directed,
                eof: true,
                bytes: Vec::new(),
            }),
            Some(_) | None => None,
        };
        if let Some(inbound) = queued
            && self.inbound.try_send(inbound).is_err()
        {
            // Full or closed: the caller is not draining. Dropping is the only
            // option that neither blocks the event loop nor grows without bound.
            tracing::warn!(
                target: "agent_habilis_mesh::messages",
                "inbound queue full; dropped a pipe frame"
            );
        }
        // Never retained or indexed — the frame is fully handled here.
        false
    }
}

#[async_trait]
impl NodeDriver for PipeApp {
    type Session = Request;
    type Http = ();
    type Ipc = ();

    async fn handle_session(
        &mut self,
        req: Request,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool {
        match req {
            Request::Send {
                tag,
                to,
                body,
                reply,
            } => {
                let sent = send_app(
                    state,
                    ctx,
                    AppFrameParams {
                        tag,
                        to,
                        corr: None,
                        body,
                    },
                )
                .await;
                let _ = reply.send(sent.map_err(|error| error.to_string()));
                true
            }
            Request::StateMerge { merge, reply } => {
                let merged = broadcast_state_merge(
                    state,
                    StateMergeParams {
                        mesh: ctx.mesh,
                        author: ctx.author,
                        merge,
                        sender: ctx.sender,
                        sink: ctx.sink,
                        channel: Channel::State,
                        surface: true,
                    },
                )
                .await;
                let _ = reply.send(merged.map(|_| ()).map_err(|error| error.to_string()));
                true
            }
            Request::StateJson { reply } => {
                let _ = reply.send(state.doc(Channel::State).to_json().to_string());
                false
            }
            Request::Peers { reply } => {
                let snapshot = state.roster_snapshot();
                let json = serde_json::to_string(&snapshot)
                    .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"));
                let _ = reply.send(json);
                false
            }
            Request::PeerCount { reply } => {
                // `peers.len()`, not the snapshot's `count` — that one includes
                // self, and a caller asking "is anyone else here?" wants zero
                // when it is alone.
                let _ = reply.send(state.roster_snapshot().peers.len());
                false
            }
        }
    }
}

/// How the caller selects a mesh — an id to join, a shared string to derive one
/// from, or a create over these lookups. Mirrors `mesh-pipe`'s flag set.
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent discovery choices (public/mdns/dht/relay) mirroring the mesh-pipe CLI; they are flat inputs, not a state machine to model as an enum"
)]
pub struct Opts {
    /// A `💬…` id to join.
    pub mesh: Option<String>,
    /// A shared string both sides derive the same public mesh from.
    pub topic: Option<String>,
    /// Local nickname; `None` mints a random one.
    pub nick: Option<String>,
    /// Create a public mesh (the all-on discovery preset).
    pub public: bool,
    pub mdns: bool,
    pub dht: bool,
    pub relay: bool,
    /// Active-view cap; `0` takes the engine default.
    pub max_peers: usize,
}

/// A live mesh membership, driven synchronously from a foreign caller's thread.
/// Owns the tokio runtime the event loop runs on.
#[expect(
    missing_debug_implementations,
    reason = "a tokio Runtime has no Debug impl; the fields a reader would want (id, nickname) are exposed as accessors"
)]
pub struct Pipe {
    runtime: tokio::runtime::Runtime,
    /// `None` after [`Pipe::close`] has taken it — `Node::leave` consumes it.
    node: Option<Node<PipeApp>>,
    inbound: mpsc::Receiver<Inbound>,
    mesh_id: String,
    nickname: String,
    chunk: usize,
}

impl Pipe {
    /// Resolve `opts`, stand up the mesh, and spawn the event loop.
    ///
    /// # Errors
    /// An unparseable id/topic/nickname, conflicting selectors, or a failure
    /// standing up the endpoint and gossip overlay.
    pub fn open(opts: &Opts) -> Result<Self> {
        let nickname = opts
            .nick
            .clone()
            .map(Nickname::new)
            .transpose()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let (kind, author) = resolve_kind(opts, nickname)?;
        let max_peers = if opts.max_peers == 0 {
            GOSSIP_ACTIVE_VIEW_CAPACITY
        } else {
            opts.max_peers
        };

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("building the mesh runtime")?;
        let (inbound_tx, inbound) = mpsc::channel(INBOUND_CAP);

        let node = runtime.block_on(async move {
            let config = setup_mesh(
                kind,
                SetupParams {
                    author,
                    max_peers,
                    // An embedded library writes no files and binds no control
                    // socket, so it claims no /tmp root of its own.
                    runtime_base: None,
                    state_file: None,
                    sink: Arc::new(SilentSink),
                    multihop: false,
                    // A byte pipe publishes no per-peer identity, so `meta`
                    // stays free-form.
                    per_peer_gate: None,
                    cohost: None,
                    live_count: None,
                },
            )
            .await
            .context("setting up the mesh")?;
            // `handle_signals: false` — the one deliberate divergence from
            // mesh-pipe, which passes `true` because it owns its process. This
            // is a library inside somebody else's: installing process-wide
            // ctrl-c / SIGTERM listeners would hijack the host's own handling.
            // A foreign caller traps signals itself and calls `mesh_close`.
            Ok::<_, anyhow::Error>(Node::spawn(
                config,
                PipeApp {
                    inbound: inbound_tx,
                },
                /* push */ None,
                /* handle_signals */ false,
            ))
        })?;

        Ok(Self {
            mesh_id: node.mesh_id().as_str().to_owned(),
            nickname: node.nickname().to_string(),
            runtime,
            node: Some(node),
            inbound,
            chunk: default_chunk(),
        })
    }

    #[must_use]
    pub fn mesh_id(&self) -> &str {
        &self.mesh_id
    }

    #[must_use]
    pub fn nickname(&self) -> &str {
        &self.nickname
    }

    /// Send `bytes` as one or more `pipe_data` frames — broadcast when `to` is
    /// `None`, directed at that peer otherwise. Splits at the single-frame
    /// budget so the caller can hand over a buffer of any size.
    ///
    /// # Errors
    /// The event loop has stopped, or the engine refused a frame.
    pub fn send(&self, to: Option<&str>, bytes: &[u8]) -> Result<()> {
        let to = parse_to(to)?;
        for slice in bytes.chunks(self.chunk) {
            let body = MessageBody::new(BASE64.encode(slice))
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            self.request_send(AppTag::from(tag::DATA), to.clone(), body)?;
        }
        Ok(())
    }

    /// Send the end-of-stream marker (an empty `pipe_eof` body).
    ///
    /// # Errors
    /// The event loop has stopped, or the engine refused the frame.
    pub fn send_eof(&self, to: Option<&str>) -> Result<()> {
        let to = parse_to(to)?;
        let body = MessageBody::new(String::new()).map_err(|error| anyhow::anyhow!("{error}"))?;
        self.request_send(AppTag::from(tag::EOF), to, body)
    }

    fn request_send(&self, tag: AppTag, to: Option<Nickname>, body: MessageBody) -> Result<()> {
        let (reply, answer) = oneshot::channel();
        self.dispatch(Request::Send {
            tag,
            to,
            body,
            reply,
        })?;
        self.await_reply(answer)?
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    /// Take the next inbound frame, waiting at most `timeout`. `Ok(None)` on
    /// timeout — distinct from a `pipe_eof` frame, which arrives as
    /// `Inbound { eof: true, .. }`.
    ///
    /// # Errors
    /// The event loop has stopped and the queue is drained.
    pub fn recv(&mut self, timeout: Duration) -> Result<Option<Inbound>> {
        let inbound = &mut self.inbound;
        self.runtime.block_on(async move {
            match tokio::time::timeout(timeout, inbound.recv()).await {
                Err(_elapsed) => Ok(None),
                Ok(Some(frame)) => Ok(Some(frame)),
                Ok(None) => Err(anyhow::anyhow!("mesh event loop has stopped")),
            }
        })
    }

    /// Apply an RFC 7386 merge document to the shared `state` channel and gossip
    /// the resulting automerge change.
    ///
    /// # Errors
    /// `json` is not a JSON object, the merge is unrepresentable, the resulting
    /// frame is oversize, or the event loop has stopped.
    pub fn state_merge(&self, json: &str) -> Result<()> {
        let merge: serde_json::Value =
            serde_json::from_str(json).context("parsing the merge document")?;
        let (reply, answer) = oneshot::channel();
        self.dispatch(Request::StateMerge { merge, reply })?;
        self.await_reply(answer)?
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    /// The merged shared `state` document as JSON.
    ///
    /// # Errors
    /// The event loop has stopped.
    pub fn state_json(&self) -> Result<String> {
        let (reply, answer) = oneshot::channel();
        self.dispatch(Request::StateJson { reply })?;
        self.await_reply(answer)
    }

    /// The live peer roster as JSON (the `agent-gossip peers` shape).
    ///
    /// # Errors
    /// The event loop has stopped.
    pub fn peers_json(&self) -> Result<String> {
        let (reply, answer) = oneshot::channel();
        self.dispatch(Request::Peers { reply })?;
        self.await_reply(answer)
    }

    /// How many peers other than us are in the mesh right now — the cheap
    /// question a caller waiting for company actually has, without a JSON
    /// round-trip through [`Pipe::peers_json`].
    ///
    /// # Errors
    /// The event loop has stopped.
    pub fn peer_count(&self) -> Result<usize> {
        let (reply, answer) = oneshot::channel();
        self.dispatch(Request::PeerCount { reply })?;
        self.await_reply(answer)
    }

    /// Broadcast `Left` and wind the loop down. Holds the same brief grace
    /// period `mesh-pipe` does first: a gossip broadcast is fire-and-forget, so
    /// leaving the instant after a send could race the frames out of existence.
    ///
    /// # Errors
    /// The event loop returned an error or panicked.
    pub fn close(&mut self) -> Result<()> {
        let Some(node) = self.node.take() else {
            return Ok(());
        };
        self.runtime.block_on(async move {
            tokio::time::sleep(DEPARTURE_GRACE).await;
            node.leave().await
        })
    }

    fn dispatch(&self, req: Request) -> Result<()> {
        let Some(node) = self.node.as_ref() else {
            anyhow::bail!("this mesh handle is closed");
        };
        self.runtime.block_on(node.send(req))
    }

    fn await_reply<T>(&self, answer: oneshot::Receiver<T>) -> Result<T> {
        self.runtime
            .block_on(answer)
            .map_err(|_| anyhow::anyhow!("mesh event loop dropped the request"))
    }
}

/// Matches `mesh-pipe`'s post-EOF wait before leaving.
const DEPARTURE_GRACE: Duration = Duration::from_millis(750);

fn parse_to(to: Option<&str>) -> Result<Option<Nickname>> {
    to.map(|nick| Nickname::new(nick.to_owned()))
        .transpose()
        .map_err(|error| anyhow::anyhow!("{error}"))
}

/// Resolve the selectors into a [`SetupKind`] plus our nickname. Exactly one
/// source: an id, a topic string, or a create over the lookup flags (no
/// selector at all ⇒ a loopback create).
fn resolve_kind(opts: &Opts, nickname: Option<Nickname>) -> Result<(SetupKind, Nickname)> {
    match (&opts.mesh, &opts.topic) {
        (Some(_), Some(_)) => anyhow::bail!("pass only one of mesh / topic"),
        (Some(id), None) => {
            let target: JoinTarget = id.parse().map_err(|error| anyhow::anyhow!("{error}"))?;
            let Resolved { kind, author, .. } = JoinParams {
                target,
                nickname,
                password: None,
            }
            .resolve()
            .context("resolving the mesh id")?;
            Ok((kind, author))
        }
        (None, Some(string)) => {
            let Resolved { kind, author, .. } = TopicParams {
                string: string.clone(),
                nickname,
            }
            .resolve()
            .context("resolving the topic string")?;
            Ok((kind, author))
        }
        (None, None) => {
            let lookups = LookupSet {
                mdns: opts.mdns,
                dht: opts.dht,
                relay: if opts.relay {
                    RelaySelection::Default
                } else {
                    RelaySelection::Unset
                },
            };
            let config = MeshConfig {
                lookups: resolve_lookups(opts.public, lookups),
                password: None,
                issuer_pubkey: None,
            };
            let name = MeshName::new("mesh-ffi").map_err(|error| anyhow::anyhow!("{error}"))?;
            let Resolved { kind, author, .. } = CreateParams {
                name,
                nickname,
                config,
                advertise: DirectorySelection::Unset,
                password: None,
                invite_only: false,
            }
            .resolve()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok((kind, author))
        }
    }
}

/// The default raw-bytes-per-frame budget, identical to `mesh-pipe`'s: a
/// `pipe_data` frame is unsharded, so the base64-inflated body plus the JSON
/// envelope must fit [`MAX_MESSAGE_SIZE`]. Invert base64's 4/3 growth after
/// reserving envelope headroom, then round to a multiple of 3 so base64 emits no
/// mid-stream padding.
///
/// Public because a foreign caller needs it to size its receive buffer: a frame
/// larger than the buffer handed to `mesh_recv` is an error, not a truncation.
#[must_use]
pub fn default_chunk() -> usize {
    const ENVELOPE_RESERVE: usize = 1024;
    let body_budget = MAX_MESSAGE_SIZE.saturating_sub(ENVELOPE_RESERVE);
    let raw = body_budget / 4 * 3;
    (raw / 3) * 3
}
