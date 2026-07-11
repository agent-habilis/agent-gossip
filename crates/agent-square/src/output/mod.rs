use tokio::sync::mpsc::UnboundedSender;

use crate::a2a::TaskId;
use agent_habilis_mesh::protocol::mesh::MeshName;
use agent_habilis_mesh::protocol::{MeshId, Message, MessageId, MessageKind, Nickname};

mod json;
#[cfg(test)]
mod tests;

pub(crate) use json::PingPeer;
pub(crate) use json::is_visible;
use json::{
    SimpleEvent, emit, emit_json, format_presence_json, peer_return_display, peer_timeout_display,
    ping_report_display, print_message_json,
};
pub use json::{event_json, surfaced_event_json};

/// Output mode — chosen per event loop at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Json,
    /// Silent mode — suppresses all stdout and stderr user-facing
    /// output. Used by the MCP server, where stdout is reserved for
    /// the JSON-RPC protocol transport and any print would corrupt
    /// the stream.
    Silent,
}

/// Owned, structured form of every line the daemon would print.
/// In-process consumers ([`crate::embed::MeshSession::events`],
/// tests) receive these over a channel instead of scraping stdout;
/// the `Stream` sink renders the *same* data through `events.rs`, so
/// the wire format stays byte-identical.
///
/// `#[non_exhaustive]`: a future event kind must not be a breaking
/// change for embedders matching on this.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OutputEvent {
    Ready {
        mesh: MeshId,
        name: MeshName,
        nickname: Nickname,
        /// One-line skill-drift warning when the installed integration has
        /// fallen behind this binary (see `cli::agent::drift_warning`), else
        /// `None`. The startup nag for stale skills.
        drift: Option<String>,
        /// The bound `--a2a-serve` port; `None` when the binding is off
        /// (the default), keeping the common event unchanged.
        a2a_port: Option<u16>,
    },
    MeshId {
        id: MeshId,
    },
    Message {
        msg: Box<Message>,
        is_self: bool,
    },
    Presence {
        msg: Box<Message>,
    },
    PeerTimeout {
        nickname: Nickname,
        last_seen_secs_ago: u64,
    },
    PeerReturn {
        nickname: Nickname,
    },
    /// Equivocation detected (Phase 2): an author key signed two different
    /// messages at the same `seq`. Surfaced once per offending key.
    Fork {
        nickname: Nickname,
        pubkey: String,
        seq: u64,
    },
    MsgPosted {
        id: MessageId,
    },
    Info {
        message: String,
    },
    Error {
        message: String,
    },
    PingReport {
        peers: Vec<PingPeer>,
        known: usize,
    },
    /// One leg of a task surfaced to the addressee (or the
    /// sender's own echo). Carries the full `Message` so the JSON sink can
    /// render `to`/`task_id`/`phase`/`body`. The JSON sink renders
    /// the `Progress` phase as a `task_progress` event and every other
    /// phase as a `task` event.
    Task {
        msg: Box<Message>,
        is_self: bool,
    },
    /// An RPC `message/send` task leg (the initiator's brief / answer, or the
    /// created `Task`) surfaced with no backing frame — it arrived over the
    /// request/response binding. Renders as a `task` event, `kind:message`.
    TaskMessage {
        id: String,
        mesh: String,
        author: String,
        peer: String,
        task_id: String,
        state: Option<crate::a2a::TaskState>,
        text: String,
        is_self: bool,
    },
    /// A task was evicted for crossing its idle-debounce timeout (the
    /// daemon-side per-task silence sweep). Daemon-originated — no `Message`.
    TaskTimeout {
        task_id: TaskId,
    },
    /// A shared-state change: the patch event, the freshly-derived document, and
    /// whether it was our own write. Drives the reaction hook and the human
    /// `💬 … changed shared state` line.
    StateChanged {
        channel: agent_habilis_mesh::protocol::Channel,
        event: Box<Message>,
        document: serde_json::Value,
        is_self: bool,
    },
}

/// The value cluster for [`Output::ready`]: the mesh identity fields plus
/// optional startup diagnostics (skill drift, `--a2a-serve` port).
#[derive(Clone, Copy)]
pub(crate) struct ReadyParams<'a> {
    pub mesh: &'a MeshId,
    pub name: &'a MeshName,
    pub nickname: &'a Nickname,
    pub drift: Option<&'a str>,
    pub a2a_port: Option<u16>,
}

/// The value cluster for [`Output::state_changed`]: the channel, the patch
/// event, the freshly-derived document, and whether it was our own write.
#[derive(Clone, Copy)]
pub(crate) struct StateChangedParams<'a> {
    pub channel: agent_habilis_mesh::protocol::Channel,
    pub event: &'a Message,
    pub document: &'a serde_json::Value,
    pub is_self: bool,
}

/// An RPC `message/send` task leg with no backing frame — the fields
/// [`Output::task_message`] and [`json::format_task_message_json`] need,
/// bundled (they mirror the [`OutputEvent::TaskMessage`] variant's fields).
pub(crate) struct TaskMessageLeg<'a> {
    pub id: &'a str,
    pub mesh: &'a str,
    pub author: &'a str,
    pub peer: &'a str,
    pub task_id: &'a str,
    pub state: Option<crate::a2a::TaskState>,
    pub text: &'a str,
    pub is_self: bool,
}

/// Per-event-loop output destination. Replaces the former
/// process-global `MODE`/`FILTER_SELF` statics so multiple in-process
/// sessions (embed facade, tests) can run with independent output
/// instead of racing a first-write-wins `OnceLock`. Threaded by
/// `&Output` (not `Copy` — `Capture` holds a channel sender).
#[derive(Debug, Clone)]
pub(crate) enum Output {
    /// Write to the process stdout/stderr in the given mode (CLI /
    /// MCP-silent).
    ///
    /// `tap` is an optional fan-out: when set, every surfaced
    /// [`OutputEvent`] is *also* sent here, in addition to being rendered
    /// to stdout. The CLI daemon attaches one so the event loop can mirror
    /// the surfaced stream into its local `surfaced_events` ring (the
    /// `poll`/`fetch` history), without each emit site needing `&mut
    /// state`. The tap carries the *same* structured events the `Capture`
    /// sink would, so a tapped event renders byte-identically through
    /// [`event_json`] later. Self-filtering still applies before the tap
    /// (a suppressed self-message is neither printed nor tapped).
    Stream {
        mode: OutputMode,
        filter_self: bool,
        tap: Option<UnboundedSender<OutputEvent>>,
    },
    /// Push structured events into an in-process channel (embed
    /// facade / tests). Mode-agnostic: every event is captured.
    ///
    /// `tap` mirrors each event into the daemon's `surfaced_events` ring,
    /// exactly as the `Stream` tap does — so the embed facade's `fetch`
    /// (pull) sees the same events its `events()` subscription (push) does.
    Capture {
        tx: UnboundedSender<OutputEvent>,
        filter_self: bool,
        tap: Option<UnboundedSender<OutputEvent>>,
    },
}

impl Output {
    pub(crate) fn new(mode: OutputMode, filter_self: bool) -> Self {
        Self::Stream {
            mode,
            filter_self,
            tap: None,
        }
    }

    /// Attach a fan-out `tap`, returning the tapped sink. Every surfaced
    /// [`OutputEvent`] is then *also* sent to `tap` (the daemon's
    /// `surfaced_events` mirror) in addition to being rendered/captured.
    /// Applies to BOTH the `Stream` sink (CLI daemon) and the `Capture` sink
    /// (embed/MCP — so its `fetch` sees the same events its `events()`
    /// subscription does); replaces any existing tap (there is only ever one
    /// attach site). Lets the event loop wrap the sink it was handed without
    /// `EventLoopConfig` carrying a receiver.
    pub(crate) fn with_tap(self, tap: UnboundedSender<OutputEvent>) -> Self {
        match self {
            Output::Stream {
                mode, filter_self, ..
            } => Output::Stream {
                mode,
                filter_self,
                tap: Some(tap),
            },
            Output::Capture {
                tx, filter_self, ..
            } => Output::Capture {
                tx,
                filter_self,
                tap: Some(tap),
            },
        }
    }

    /// Suppresses all stdout/stderr output (MCP server).
    pub(crate) fn silent() -> Self {
        Self::Stream {
            mode: OutputMode::Silent,
            filter_self: false,
            tap: None,
        }
    }

    /// Capture every event into `tx` (embed facade / in-process
    /// tests).
    pub(crate) fn capture(tx: UnboundedSender<OutputEvent>) -> Self {
        Self::Capture {
            tx,
            filter_self: false,
            tap: None,
        }
    }

    fn filters_self(&self) -> bool {
        match self {
            Self::Stream { filter_self, .. } | Self::Capture { filter_self, .. } => *filter_self,
        }
    }

    /// The one sink skeleton every event method shares: in `Capture`
    /// mode push the structured [`OutputEvent`]; in `Stream` mode render
    /// that event's own human / JSON / (silent) form, and — when a `tap`
    /// is attached — *also* push the structured event so the daemon's
    /// `surfaced_events` ring sees the identical stream. Factoring this
    /// out is what removes the ~10× duplicated `match Stream/Capture`.
    fn dispatch(&self, event: impl FnOnce() -> OutputEvent, stream: impl FnOnce(OutputMode)) {
        match self {
            Output::Stream { mode, tap, .. } => {
                stream(*mode);
                if let Some(tap) = tap {
                    let _ = tap.send(event());
                }
            }
            Output::Capture { tx, tap, .. } => {
                // Build once; the live `events()` subscription and the
                // `surfaced_events` ring (tap) each get a copy.
                let event = event();
                if let Some(tap) = tap {
                    let _ = tap.send(event.clone());
                }
                let _ = tx.send(event);
            }
        }
    }

    pub(crate) fn ready(&self, params: ReadyParams<'_>) {
        let ReadyParams {
            mesh,
            name,
            nickname,
            drift,
            a2a_port,
        } = params;
        self.dispatch(
            || OutputEvent::Ready {
                mesh: mesh.clone(),
                name: name.clone(),
                nickname: nickname.clone(),
                drift: drift.map(str::to_owned),
                a2a_port,
            },
            |mode| {
                if mode == OutputMode::Json {
                    emit_json(
                        &SimpleEvent::Ready {
                            version: crate::VERSION,
                            square: mesh.as_str(),
                            name: name.as_str(),
                            nickname: nickname.as_str(),
                            drift,
                            a2a_port,
                        },
                        false,
                    );
                }
            },
        );
    }

    /// Surface the mesh identifier at startup (stderr). JSON mode prints the
    /// bare `💬…` id (the integration harness greps this); Silent suppresses it.
    pub(crate) fn mesh_id_line(&self, id: &MeshId) {
        self.dispatch(
            || OutputEvent::MeshId { id: id.clone() },
            |mode| match mode {
                OutputMode::Json => eprintln!("{id}"),
                OutputMode::Silent => {}
            },
        );
    }

    pub(crate) fn print_message(&self, msg: &Message) {
        self.print_message_ex(msg, false);
    }

    /// Print a message with an optional self flag (for echo-back).
    /// When `filter_self` is enabled, self-authored messages are
    /// suppressed. Presence events should use `print_presence` instead.
    pub(crate) fn print_message_ex(&self, msg: &Message, is_self: bool) {
        if matches!(
            msg.kind,
            MessageKind::PeerInfo
                | MessageKind::Digest
                | MessageKind::StateDigest
                | MessageKind::MetaDigest
                | MessageKind::Ping
                | MessageKind::Pong { .. }
        ) {
            return;
        }
        if is_self && self.filters_self() {
            return;
        }
        self.dispatch(
            || OutputEvent::Message {
                msg: Box::new(msg.clone()),
                is_self,
            },
            |mode| match mode {
                OutputMode::Json => print_message_json(msg, is_self),
                OutputMode::Silent => {}
            },
        );
    }

    /// Surface a worker-pushed task frame (an `a2a_status` or `a2a_artifact`).
    /// Distinct top-level `task` event (not the `message` family) so skills
    /// branch on `event`. When `filter_self` is enabled, self-authored legs are
    /// suppressed.
    pub(crate) fn print_task(&self, msg: &Message, is_self: bool) {
        // Only worker-pushed status/artifact frames surface as a `task` event.
        match &msg.kind {
            MessageKind::App { tag, to, .. } => match (tag.as_str(), to) {
                (crate::a2a::wire::STATUS | crate::a2a::wire::ARTIFACT, Some(_)) => {}
                _ => return,
            },
            MessageKind::Presence { .. }
            | MessageKind::PeerInfo
            | MessageKind::Digest
            | MessageKind::StateDigest
            | MessageKind::MetaDigest
            | MessageKind::Ping
            | MessageKind::Pong { .. }
            | MessageKind::State
            | MessageKind::Meta
            | MessageKind::LinkState => return,
        }
        if is_self && self.filters_self() {
            return;
        }
        self.dispatch(
            || OutputEvent::Task {
                msg: Box::new(msg.clone()),
                is_self,
            },
            |mode| match mode {
                OutputMode::Json => json::print_task_json(msg, is_self),
                OutputMode::Silent => {}
            },
        );
    }

    /// Surface an RPC `message/send` task leg — the initiator's brief / answer
    /// (on the worker), or the created `Task` (on the initiator). No frame
    /// backs it (the leg arrived over the request/response binding), so the
    /// fields are passed explicitly. Emits a `task` event with `kind:message`.
    pub(crate) fn task_message(&self, leg: &TaskMessageLeg<'_>) {
        if leg.is_self && self.filters_self() {
            return;
        }
        let state = leg.state;
        let is_self = leg.is_self;
        let (id, mesh, author, peer, task_id, text) = (
            leg.id.to_owned(),
            leg.mesh.to_owned(),
            leg.author.to_owned(),
            leg.peer.to_owned(),
            leg.task_id.to_owned(),
            leg.text.to_owned(),
        );
        self.dispatch(
            || OutputEvent::TaskMessage {
                id: id.clone(),
                mesh: mesh.clone(),
                author: author.clone(),
                peer: peer.clone(),
                task_id: task_id.clone(),
                state,
                text: text.clone(),
                is_self,
            },
            |mode| match mode {
                OutputMode::Json => emit(&json::format_task_message_json(&TaskMessageLeg {
                    id: &id,
                    mesh: &mesh,
                    author: &author,
                    peer: &peer,
                    task_id: &task_id,
                    state,
                    text: &text,
                    is_self,
                })),
                OutputMode::Silent => {}
            },
        );
    }

    /// A task crossed its idle-debounce timeout and was evicted by the
    /// daemon's per-task silence sweep (the task analogue of
    /// [`peer_timeout`](Self::peer_timeout)). JSON mode emits a `task_timeout`
    /// event for the widget.
    pub(crate) fn task_timeout(&self, task_id: &TaskId) {
        self.dispatch(
            || OutputEvent::TaskTimeout {
                task_id: task_id.clone(),
            },
            |mode| match mode {
                OutputMode::Json => emit_json(
                    &SimpleEvent::TaskTimeout {
                        task_id: task_id.as_str(),
                    },
                    false,
                ),
                OutputMode::Silent => {}
            },
        );
    }

    /// Surface a shared-state change: the human `💬 … changed shared state`
    /// line, the structured `state` event for the agent API (poll / `--output
    /// json` / embed `events()`), and the surfaced-ring mirror. F5: a *self*
    /// change goes to poll/UI but is **never** delivered to the embed `events()`
    /// reaction channel, so an agent is never woken by its own patch (alternation
    /// stays loop-safe without a per-consumer guard). On the CLI/Monitor path the
    /// self-skip is the Monitor's job.
    pub(crate) fn state_changed(&self, params: StateChangedParams<'_>) {
        let StateChangedParams {
            channel,
            event,
            document,
            is_self,
        } = params;
        let make = || OutputEvent::StateChanged {
            channel,
            event: Box::new(event.clone()),
            document: document.clone(),
            is_self,
        };
        match self {
            Output::Stream { mode, tap, .. } => {
                match mode {
                    OutputMode::Json => {
                        emit(&json::format_state_json(channel, event, document, is_self));
                    }
                    OutputMode::Silent => {}
                }
                if let Some(tap) = tap {
                    let _ = tap.send(make());
                }
            }
            Output::Capture { tx, tap, .. } => {
                let evt = make();
                // Poll/fetch history always sees it.
                if let Some(tap) = tap {
                    let _ = tap.send(evt.clone());
                }
                // F5: never wake an agent on its own patch.
                if !is_self {
                    let _ = tx.send(evt);
                }
            }
        }
    }

    pub(crate) fn print_presence(&self, msg: &Message) {
        let MessageKind::Presence { subtype } = &msg.kind else {
            return;
        };
        self.dispatch(
            || OutputEvent::Presence {
                msg: Box::new(msg.clone()),
            },
            |mode| match mode {
                OutputMode::Json => emit(&format_presence_json(msg, *subtype)),
                OutputMode::Silent => {}
            },
        );
    }

    /// Emit a peer-timeout event. Distinct from `presence left` (a
    /// voluntary departure): this fires when the local heartbeat
    /// sweeper evicts a participant silent past `ALIVE_TIMEOUT_SECS`.
    pub(crate) fn peer_timeout(&self, nickname: &Nickname, last_seen_secs_ago: u64) {
        self.dispatch(
            || OutputEvent::PeerTimeout {
                nickname: nickname.clone(),
                last_seen_secs_ago,
            },
            |mode| match mode {
                OutputMode::Json => emit_json(
                    &SimpleEvent::PeerTimeout {
                        nickname: nickname.as_str(),
                        last_seen_secs_ago,
                        display: peer_timeout_display(nickname.as_str()),
                    },
                    true,
                ),
                OutputMode::Silent => {}
            },
        );
    }

    /// Equivocation / fork alert (Phase 2): an author's signing key produced
    /// two different messages at the same `seq` — cryptographic proof of a
    /// fork. Surfaced once per offending key. JSON mode emits a `fork` event
    /// for agents.
    pub(crate) fn fork(&self, nickname: &Nickname, pubkey: &str, seq: u64) {
        self.dispatch(
            || OutputEvent::Fork {
                nickname: nickname.clone(),
                pubkey: pubkey.to_owned(),
                seq,
            },
            |mode| match mode {
                OutputMode::Json => emit_json(
                    &SimpleEvent::Fork {
                        nickname: nickname.as_str(),
                        pubkey,
                        seq,
                    },
                    false,
                ),
                OutputMode::Silent => {}
            },
        );
    }

    /// Symmetric counterpart to `peer_timeout`: fires when a peer we
    /// previously evicted for silence resumes sending (any message,
    /// including an `Alive` keepalive).
    pub(crate) fn peer_return(&self, nickname: &Nickname) {
        self.dispatch(
            || OutputEvent::PeerReturn {
                nickname: nickname.clone(),
            },
            |mode| match mode {
                OutputMode::Json => emit_json(
                    &SimpleEvent::PeerReturn {
                        nickname: nickname.as_str(),
                        display: peer_return_display(nickname.as_str()),
                    },
                    true,
                ),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print a "message posted" confirmation with the message ID.
    pub(crate) fn msg_posted(&self, id: &MessageId) {
        self.dispatch(
            || OutputEvent::MsgPosted { id: id.clone() },
            |mode| match mode {
                OutputMode::Json => emit_json(&SimpleEvent::MsgPosted { id: id.as_str() }, false),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print an informational line.
    pub(crate) fn info(&self, msg: &str) {
        self.dispatch(
            || OutputEvent::Info {
                message: msg.to_owned(),
            },
            |mode| match mode {
                OutputMode::Json => emit_json(&SimpleEvent::Info { message: msg }, false),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print an error line.
    pub(crate) fn error(&self, msg: &str) {
        self.dispatch(
            || OutputEvent::Error {
                message: msg.to_owned(),
            },
            |mode| match mode {
                OutputMode::Json => emit_json(&SimpleEvent::Error { message: msg }, false),
                OutputMode::Silent => {}
            },
        );
    }

    /// Emit the result of an `agent-square ping` round: per-peer RTT, plus how
    /// many of the known peers responded (the responder count is just
    /// `peers.len()`). `known` is the current participant roster size.
    pub(crate) fn ping_report(&self, peers: Vec<PingPeer>, known: usize) {
        // Not routed through `dispatch`: `peers` is owned and each arm
        // consumes (or drops) it, so there is nothing to clone.
        match self {
            Output::Capture { tx, tap, .. } => {
                if let Some(tap) = tap {
                    let _ = tap.send(OutputEvent::PingReport {
                        peers: peers.clone(),
                        known,
                    });
                }
                let _ = tx.send(OutputEvent::PingReport { peers, known });
            }
            Output::Stream { mode, tap, .. } => {
                match mode {
                    OutputMode::Json => emit_json(
                        &SimpleEvent::PingReport {
                            responded: peers.len(),
                            display: ping_report_display(&peers, known),
                            peers: peers.clone(),
                            known,
                        },
                        true,
                    ),
                    OutputMode::Silent => {}
                }
                // Mirror into the daemon's surfaced-events ring so `poll`
                // can return the same `ping_report` the stream just showed.
                if let Some(tap) = tap {
                    let _ = tap.send(OutputEvent::PingReport { peers, known });
                }
            }
        }
    }
}

/// The app renders the engine's generic
/// [`NodeEvent`](agent_habilis_mesh::gossip::event::NodeEvent)s by mapping each onto the
/// existing `Output` method, so the stdout JSON and tap forms stay
/// byte-identical. This seam lets the engine emit surfacings without naming the
/// concrete `Output`.
impl agent_habilis_mesh::gossip::event::NodeSink for Output {
    fn emit(&self, event: agent_habilis_mesh::gossip::event::NodeEvent) {
        use agent_habilis_mesh::gossip::event::NodeEvent;
        match event {
            NodeEvent::Ready {
                mesh,
                name,
                nickname,
                drift,
                a2a_port,
            } => self.ready(ReadyParams {
                mesh: &mesh,
                name: &name,
                nickname: &nickname,
                drift: drift.as_deref(),
                a2a_port,
            }),
            NodeEvent::MeshId { id } => self.mesh_id_line(&id),
            NodeEvent::Info(message) => self.info(&message),
            NodeEvent::Error(message) => self.error(&message),
            NodeEvent::Fork {
                nickname,
                pubkey,
                seq,
            } => self.fork(&nickname, &pubkey, seq),
            NodeEvent::PeerTimeout {
                nickname,
                last_seen_secs_ago,
            } => self.peer_timeout(&nickname, last_seen_secs_ago),
            NodeEvent::PeerReturn { nickname } => self.peer_return(&nickname),
            NodeEvent::Presence { msg } => self.print_presence(&msg),
            NodeEvent::PingReport { peers, known } => self.ping_report(
                peers
                    .into_iter()
                    .map(|peer| PingPeer {
                        nickname: peer.nickname,
                        rtt_ms: peer.rtt_ms,
                    })
                    .collect(),
                known,
            ),
            NodeEvent::StateChanged {
                channel,
                event,
                document,
                is_self,
            } => self.state_changed(StateChangedParams {
                channel,
                event: &event,
                document: &document,
                is_self,
            }),
        }
    }
}
