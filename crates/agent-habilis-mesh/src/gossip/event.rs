use crate::protocol::mesh::MeshName;
use crate::protocol::{Channel, MeshId, Message, Nickname};

/// One peer's round-trip time in a [`NodeEvent::PingReport`] — the engine's
/// chat-agnostic ping datum. The app maps it onto its `output::PingPeer` for
/// rendering, so the engine never names the app's public ping type.
#[derive(Debug, Clone)]
pub struct PingRtt {
    pub nickname: Nickname,
    pub rtt_ms: u64,
}

/// A generic surfacing the engine emits — the chat-agnostic subset of the app's
/// `OutputEvent`. The app's [`NodeSink`] impl maps each variant onto the
/// existing `Output` method, so the rendered stdout / `--output json` / embed
/// forms stay byte-identical.
#[derive(Debug)]
pub enum NodeEvent {
    Ready {
        mesh: MeshId,
        name: MeshName,
        nickname: Nickname,
        drift: Option<String>,
        a2a_port: Option<u16>,
    },
    MeshId {
        id: MeshId,
    },
    Info(String),
    Error(String),
    Fork {
        nickname: Nickname,
        pubkey: String,
        seq: u64,
    },
    PeerTimeout {
        nickname: Nickname,
        last_seen_secs_ago: u64,
    },
    PeerReturn {
        nickname: Nickname,
    },
    Presence {
        msg: Box<Message>,
    },
    PingReport {
        peers: Vec<PingRtt>,
        known: usize,
    },
    StateChanged {
        channel: Channel,
        event: Box<Message>,
        document: serde_json::Value,
        is_self: bool,
    },
}

/// The sink the engine emits generic [`NodeEvent`]s through. The app implements
/// it (backed by its `Output` renderer); the engine holds only a `&dyn NodeSink`
/// / `Arc<dyn NodeSink>` and never names the concrete sink.
pub trait NodeSink: Send + Sync {
    fn emit(&self, event: NodeEvent);
}

/// A sink that drops every [`NodeEvent`]. The no-op emitter for an embedder that
/// renders nothing from the engine's transport/membership surfacings (e.g. a
/// bytes-piping consumer that only reacts to its own `App` frames), and for
/// tests that need a sink handle without asserting on it.
#[derive(Debug, Default, Clone, Copy)]
pub struct SilentSink;

impl NodeSink for SilentSink {
    fn emit(&self, _event: NodeEvent) {}
}

/// A test sink that counts emitted [`NodeEvent`]s without inspecting them —
/// enough to assert an engine surfacing fired (and how many times) without
/// pulling the app's `Output` into an engine test.
#[cfg(test)]
pub(crate) struct CountingSink(std::sync::atomic::AtomicUsize);

#[cfg(test)]
impl CountingSink {
    pub(crate) fn new() -> Self {
        Self(std::sync::atomic::AtomicUsize::new(0))
    }

    pub(crate) fn count(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
impl NodeSink for CountingSink {
    fn emit(&self, _event: NodeEvent) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
