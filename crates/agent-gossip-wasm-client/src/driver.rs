//! The application seam the engine drives, and the buffer the page polls.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use fofoca::embed::{AppClass, EventLoopState, HandlerCtx, InboundApp, NodeApp, NodeDriver};
use fofoca::ops::deliver;
use fofoca::protocol::{
    AppFrameParams, AppTag, Message, MessageBody, MessageId, Nickname,
};
use serde::Serialize;

use crate::wire;

/// What the page asks the loop to do. Opaque to the engine.
pub(crate) enum Request {
    Broadcast(String),
}

#[derive(Clone, Serialize)]
pub(crate) struct ChatMessage {
    pub seq: u32,
    pub from: String,
    pub text: String,
    /// `broadcast` or `system`, so the page can render them differently.
    pub kind: &'static str,
}

/// Borrowed for serialization, so a roster poll allocates no `serde_json::Map`
/// per peer.
#[derive(Serialize)]
struct Peer<'a> {
    nickname: &'a str,
}

#[derive(Default)]
struct Buffered {
    messages: Vec<ChatMessage>,
    peers: BTreeSet<String>,
    next_seq: u32,
}

/// The page's view of the mesh: written by the event loop, read from JS.
///
/// A mutex rather than a channel because the reader polls — it wants
/// "everything since seq N" on demand, not a stream it must drain to stay
/// current. A tab backgrounded for a minute then asking once must not have
/// missed anything.
#[derive(Clone)]
pub(crate) struct Inbox {
    self_nick: String,
    inner: Arc<Mutex<Buffered>>,
}

impl Inbox {
    pub(crate) fn new(self_nick: String) -> Self {
        Self {
            self_nick,
            inner: Arc::new(Mutex::new(Buffered::default())),
        }
    }

    /// One place that decides what a poisoned lock means here: nothing to do.
    /// The buffer is a view for the page, so losing an update is preferable to
    /// taking the tab down, and five hand-written `if let Ok` arms disagreeing
    /// about that is worse than one that does not.
    fn with<T>(&self, body: impl FnOnce(&mut Buffered) -> T) -> Option<T> {
        self.inner.lock().ok().map(|mut buffered| body(&mut buffered))
    }

    fn push(&self, from: String, text: String, kind: &'static str) {
        self.with(|buffered| {
            buffered.next_seq += 1;
            let seq = buffered.next_seq;
            buffered.messages.push(ChatMessage {
                seq,
                from,
                text,
                kind,
            });
        });
    }

    fn note_peer(&self, nickname: &str) {
        if nickname == self.self_nick {
            return;
        }
        // `contains` first so the common case — a roster that has not changed —
        // costs a lookup rather than an allocation.
        self.with(|buffered| {
            if !buffered.peers.contains(nickname) {
                buffered.peers.insert(nickname.to_owned());
            }
        });
    }

    fn drop_peer(&self, nickname: &str) {
        self.with(|buffered| buffered.peers.remove(nickname));
    }

    pub(crate) fn messages_json(&self, after: u32) -> String {
        self.with(|buffered| {
            // The buffer is append-only and sorted by seq, so the fresh tail is
            // a binary search rather than a scan of everything ever received —
            // which the page asks for twice a second, forever.
            let from = buffered.messages.partition_point(|message| message.seq <= after);
            let fresh = &buffered.messages[from..];
            if fresh.is_empty() {
                return "[]".to_owned();
            }
            serde_json::to_string(fresh).unwrap_or_else(|_| "[]".to_owned())
        })
        .unwrap_or_else(|| "[]".to_owned())
    }

    pub(crate) fn peers_json(&self) -> String {
        self.with(|buffered| {
            let peers: Vec<Peer<'_>> = buffered
                .peers
                .iter()
                .map(|nickname| Peer { nickname })
                .collect();
            serde_json::to_string(&peers).unwrap_or_else(|_| "[]".to_owned())
        })
        .unwrap_or_else(|| "[]".to_owned())
    }
}

/// The A2A payload a broadcast frame carries, pinned by the app crate's
/// `snap_a2a_message` snapshot.
///
/// `messageId` must equal the frame's own id and `contextId` the mesh id — the
/// receiving side checks both and drops the frame otherwise, which is why this
/// is built around a pre-minted id rather than through `ops::send_app` (which
/// mints its id internally, leaving nothing to reference).
#[derive(Serialize)]
struct A2aMessage<'a> {
    #[serde(rename = "messageId")]
    message_id: &'a str,
    role: &'a str,
    parts: Vec<A2aPart<'a>>,
    #[serde(rename = "contextId")]
    context_id: &'a str,
    extensions: Vec<&'a str>,
}

#[derive(Serialize)]
struct A2aPart<'a> {
    text: &'a str,
}

pub(crate) struct GossipDriver {
    inbox: Inbox,
}

impl GossipDriver {
    pub(crate) fn new(inbox: Inbox) -> Self {
        Self { inbox }
    }
}

#[async_trait::async_trait]
impl NodeApp for GossipDriver {
    fn classify(&self, message: &Message) -> AppClass {
        let tag = message.kind.app_tag().map(AppTag::as_str);
        let is_broadcast = tag == Some(wire::BROADCAST);
        AppClass {
            loggable: is_broadcast,
            beat: false,
            // Anything outside the taxonomy this client speaks is dropped whole
            // rather than surfaced: a signed frame with an arbitrary tag must
            // not reach the page.
            valid: is_broadcast || tag.is_none(),
            // Only broadcast chat carries seq/parents and joins the DAG.
            chained: is_broadcast,
            // Broadcast chat is public by definition. Directed tags are sealed,
            // and this client does not send them yet — see `Request`.
            sealed: false,
        }
    }

    async fn on_app_frame(
        &mut self,
        frame: InboundApp<'_>,
        _state: &mut EventLoopState,
        _ctx: &HandlerCtx<'_>,
    ) -> bool {
        let message = frame.message;
        if message.kind.app_tag().map(AppTag::as_str) != Some(wire::BROADCAST) {
            // Not chat. Leave it to the engine's retain/index, and do not put
            // it in front of a person as if it were a message.
            return true;
        }
        let author = message.author.as_str().to_owned();
        self.inbox.note_peer(&author);
        if let Some(text) = broadcast_text(message.body.as_str()) {
            self.inbox.push(author, text, "broadcast");
        }
        true
    }

    async fn on_peer_left(
        &mut self,
        nickname: &Nickname,
        _state: &mut EventLoopState,
        _ctx: &HandlerCtx<'_>,
    ) {
        self.inbox.drop_peer(nickname.as_str());
    }
}

#[async_trait::async_trait]
impl NodeDriver for GossipDriver {
    type Session = Request;
    type Http = ();
    type Ipc = ();

    async fn on_tick(&mut self, state: &mut EventLoopState, _ctx: &HandlerCtx<'_>) {
        // The roster is the engine's, so it is read from there rather than
        // tracked in parallel — a second copy would drift the moment a peer
        // dropped on a silence timeout rather than leaving gracefully.
        for nickname in state.peers() {
            self.inbox.note_peer(nickname.as_str());
        }
    }

    async fn handle_session(
        &mut self,
        req: Self::Session,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool {
        let Request::Broadcast(text) = req;

        let id = MessageId::random();
        let payload = A2aMessage {
            message_id: id.as_str(),
            // Chat is role user; the receiver rejects an agent role outright.
            role: "ROLE_USER",
            parts: vec![A2aPart { text: &text }],
            context_id: ctx.mesh.as_str(),
            extensions: vec![wire::EXT_MESH_BROADCAST],
        };
        let Ok(json) = serde_json::to_string(&payload) else {
            return false;
        };
        let Ok(body) = MessageBody::new(json) else {
            return false;
        };

        let frame = Message::new_app(
            ctx.mesh,
            ctx.author,
            AppFrameParams {
                tag: AppTag::from(wire::BROADCAST),
                to: None,
                corr: None,
                body,
            },
        )
        .with_id(id)
        .signed(ctx.identity);

        let Ok(bytes) = frame.serialize() else {
            return false;
        };
        if let Err(error) = deliver(&frame, bytes.into(), state, ctx.sender).await {
            self.inbox
                .push("system".to_owned(), format!("could not send: {error}"), "system");
            return false;
        }

        // Echoed locally because a broadcast does not come back to its author:
        // without this the sender is the one peer that cannot see what it said.
        self.inbox
            .push(self.inbox.self_nick.clone(), text, "broadcast");
        true
    }
}

/// Pull the text out of an A2A chat payload, tolerating a body this client did
/// not write — every peer's frame is another program's output.
fn broadcast_text(body: &str) -> Option<String> {
    let payload: serde_json::Value = serde_json::from_str(body).ok()?;
    let parts = payload.get("parts")?.as_array()?;
    let text: String = parts
        .iter()
        .filter_map(|part| part.get("text")?.as_str())
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() { None } else { Some(text) }
}
