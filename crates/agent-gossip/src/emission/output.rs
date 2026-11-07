use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use habilis_network::protocol::messages::Authored;
use habilis_network::protocol::{AuthorKey, MessageKind, Nickname};
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::error::TrySendError;

use super::{
    OutputEvent, OutputMode, PingPeer, ReadyParams, ReceiptParams, SimpleEvent, StateChangedParams,
    TaskGoneReason, TaskId, TaskMessageLeg, emit, emit_json, format_presence_json,
    peer_return_display, peer_timeout_display, ping_report_display, print_message_json,
    simple_event, task_stalled_event,
};
use crate::a2a::task_record::StallNotice;
use crate::a2a::{node, wire};

/// Per-event-loop output destination. Replaces the former
/// process-global `MODE`/`FILTER_SELF` statics so multiple in-process
/// sessions (library api, tests) can run with independent output
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
    /// [`event_json`](fn@crate::event_json) later. Self-filtering still applies before the tap
    /// (a suppressed self-message is neither printed nor tapped).
    Stream {
        mode: OutputMode,
        filter_self: bool,
        tap: Option<SurfacedTap>,
    },
    /// Push structured events into an in-process channel (the api
    /// facade / tests). Mode-agnostic: every event is captured.
    ///
    /// `tap` mirrors each event into the daemon's `surfaced_events` ring,
    /// exactly as the `Stream` tap does — so the api's `fetch`
    /// (pull) sees the same events its `events()` subscription (push) does.
    Capture {
        tx: UnboundedSender<OutputEvent>,
        filter_self: bool,
        tap: Option<SurfacedTap>,
    },
}

/// The sender end of the tap that feeds the surfaced ring: a bounded channel, so a loop that does not
/// turn cannot make it hold events without end. An event that finds it full is dropped and counted,
/// and the loop turns the count into one marker in the ring (see `A2aApp::drain_surfaced_ring`).
#[derive(Clone, Debug)]
pub(crate) struct SurfacedTap {
    events: mpsc::Sender<OutputEvent>,
    lost: Arc<AtomicU64>,
}

impl SurfacedTap {
    pub(crate) fn channel(capacity: usize) -> (Self, mpsc::Receiver<OutputEvent>) {
        let (events, receiver) = mpsc::channel(capacity);
        let tap = Self {
            events,
            lost: Arc::new(AtomicU64::new(0)),
        };
        (tap, receiver)
    }

    /// An event the ring never keeps is not queued and not counted: a burst of them must not fill
    /// the channel or inflate the loss count.
    pub(crate) fn send(&self, event: OutputEvent) {
        if !node::is_pollable(&event) {
            return;
        }
        if let Err(TrySendError::Full(_)) = self.events.try_send(event) {
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The events dropped since the last call.
    pub(crate) fn take_lost(&self) -> u64 {
        self.lost.swap(0, Ordering::Relaxed)
    }
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
    /// (in-process — so its `fetch` sees the same events its `events()`
    /// subscription does); replaces any existing tap (there is only ever one
    /// attach site). Lets the event loop wrap the sink it was handed without
    /// `EventLoopConfig` carrying a receiver.
    pub(crate) fn set_tap(self, tap: SurfacedTap) -> Self {
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

    /// Capture every event into `tx` (the library API / in-process tests).
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
                    tap.send(event());
                }
            }
            Output::Capture { tx, tap, .. } => {
                // Build once; the live `events()` subscription and the
                // `surfaced_events` ring (tap) each get a copy.
                let event = event();
                if let Some(tap) = tap {
                    tap.send(event.clone());
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
                            version: crate::version(),
                            gossip: mesh.as_str(),
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

    pub(crate) fn print_message(&self, message: &Authored, addressee_name: Option<&Nickname>) {
        self.print_message_ex(message, false, addressee_name);
    }

    /// Print a message with an optional self flag (for echo-back).
    /// When `filter_self` is enabled, self-authored messages are
    /// suppressed. Presence events should use `print_presence` instead.
    pub(crate) fn print_message_ex(
        &self,
        message: &Authored,
        is_self: bool,
        addressee_name: Option<&Nickname>,
    ) {
        if matches!(
            message.kind,
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
                message: Box::new(message.clone()),
                is_self,
                to_name: addressee_name.cloned(),
            },
            |mode| match mode {
                OutputMode::Json => print_message_json(message, is_self, addressee_name),
                OutputMode::Silent => {}
            },
        );
    }

    /// Surface a worker-pushed task frame (an `a2a_status` or `a2a_artifact`).
    /// Distinct top-level `task` event (not the `message` family) so skills
    /// branch on `event`. When `filter_self` is enabled, self-authored legs are
    /// suppressed.
    pub(crate) fn print_task(
        &self,
        message: &Authored,
        is_self: bool,
        addressee_name: Option<&Nickname>,
    ) {
        // Only worker-pushed status/artifact frames surface as a `task` event.
        let MessageKind::App {
            tag, to: Some(_), ..
        } = &message.kind
        else {
            return;
        };
        if !matches!(tag.as_str(), wire::STATUS | wire::ARTIFACT) {
            return;
        }
        if is_self && self.filters_self() {
            return;
        }
        self.dispatch(
            || OutputEvent::Task {
                message: Box::new(message.clone()),
                is_self,
                to_name: addressee_name.cloned(),
            },
            |mode| match mode {
                OutputMode::Json => simple_event::print_task_json(message, is_self, addressee_name),
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
        let id = leg.id.to_owned();
        let mesh = leg.mesh.to_owned();
        let author = leg.author.to_owned();
        let author_key = leg.author_key;
        let peer = leg.peer.to_owned();
        let task_id = leg.task_id.to_owned();
        let text = leg.text.to_owned();
        let label = leg.label.map(ToOwned::to_owned);
        let message = leg.message.cloned().map(Box::new);
        self.dispatch(
            || OutputEvent::TaskMessage {
                id,
                mesh,
                author,
                author_key,
                peer,
                task_id,
                state,
                text,
                label,
                message,
                is_self,
            },
            |mode| match mode {
                OutputMode::Json => {
                    emit(&simple_event::format_task_message_json(leg));
                }
                OutputMode::Silent => {}
            },
        );
    }

    /// A task went terminal without a peer-authored status: evicted by the
    /// daemon's per-task silence sweep (the task analogue of
    /// [`peer_timeout`](Self::peer_timeout)) or canceled because its
    /// counterparty left. JSON mode emits a `task_timeout` event for the
    /// widget; `reason` says which trigger fired.
    pub(crate) fn task_timeout(&self, task_id: &TaskId, reason: TaskGoneReason) {
        self.dispatch(
            || OutputEvent::TaskTimeout {
                task_id: task_id.clone(),
                reason,
            },
            |mode| match mode {
                OutputMode::Json => emit_json(
                    &SimpleEvent::TaskTimeout {
                        task_id: task_id.as_str(),
                        reason,
                    },
                    false,
                ),
                OutputMode::Silent => {}
            },
        );
    }

    /// A worker's agent has not polled for `check_secs` with no bell armed. JSON
    /// mode emits a visible `task_stalled` event; nothing is canceled.
    pub(crate) fn task_stalled(&self, notice: &StallNotice) {
        self.dispatch(
            || OutputEvent::TaskStalled {
                task_id: notice.task_id.clone(),
                peer: notice.peer.clone(),
                last_poll_secs_ago: notice.last_poll_secs,
                check_secs: notice.check_secs,
            },
            |mode| match mode {
                OutputMode::Json => emit_json(
                    &task_stalled_event(
                        &notice.task_id,
                        &notice.peer,
                        notice.last_poll_secs,
                        notice.check_secs,
                    ),
                    true,
                ),
                OutputMode::Silent => {}
            },
        );
    }

    /// Surface a shared-state change: the human ` … changed shared state`
    /// line, the structured `state` event for the agent API (poll / `--output
    /// json` / api `events()`), and the surfaced-ring mirror. F5: a *self*
    /// change goes to poll/UI but is **never** delivered to the api `events()`
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
        let make = |keep_document: bool| OutputEvent::StateChanged {
            channel,
            event: Box::new(event.clone()),
            document: keep_document.then(|| Arc::clone(document)),
            is_self,
        };
        // The ring keeps what a poll reader gets (see `poll_carries_document`). The live stream
        // keeps the document of every change, for in-process readers. Every copy keeps the event
        // with its body, the change itself.
        let ring_keeps_document = simple_event::poll_carries_document(channel);
        match self {
            Output::Stream { mode, tap, .. } => {
                match mode {
                    OutputMode::Json => {
                        emit(&simple_event::format_state_json(
                            channel,
                            event,
                            Some(document),
                            is_self,
                        ));
                    }
                    OutputMode::Silent => {}
                }
                if let Some(tap) = tap {
                    tap.send(make(ring_keeps_document));
                }
            }
            Output::Capture { tx, tap, .. } => {
                // Poll/fetch history always sees it.
                if let Some(tap) = tap {
                    tap.send(make(ring_keeps_document));
                }
                // F5: never wake an agent on its own patch.
                if !is_self {
                    let _ = tx.send(make(true));
                }
            }
        }
    }

    /// `peer` is the key of the member that the presence is about. A synthetic presence is signed by
    /// this node, so the key of its frame is ours and says nothing about the peer.
    pub(crate) fn print_presence(&self, message: &Authored, peer: &AuthorKey) {
        let MessageKind::Presence { subtype } = &message.kind else {
            return;
        };
        self.dispatch(
            || OutputEvent::Presence {
                message: Box::new(message.clone()),
                peer: *peer,
            },
            |mode| match mode {
                OutputMode::Json => emit(&format_presence_json(message, *subtype, peer)),
                OutputMode::Silent => {}
            },
        );
    }

    /// Emit a peer-timeout event. Distinct from `presence left` (a
    /// voluntary departure): this fires when the local heartbeat
    /// sweeper evicts a peer silent past `ALIVE_TIMEOUT_SECS`.
    pub(crate) fn peer_timeout(
        &self,
        nickname: &Nickname,
        key: &AuthorKey,
        last_seen_secs_ago: u64,
    ) {
        self.dispatch(
            || OutputEvent::PeerTimeout {
                nickname: nickname.clone(),
                key: *key,
                last_seen_secs_ago,
            },
            |mode| match mode {
                OutputMode::Json => emit_json(
                    &SimpleEvent::PeerTimeout {
                        nickname: nickname.as_str(),
                        pubkey: key.to_string().as_str(),
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
    pub(crate) fn peer_return(&self, nickname: &Nickname, key: &AuthorKey) {
        self.dispatch(
            || OutputEvent::PeerReturn {
                nickname: nickname.clone(),
                key: *key,
            },
            |mode| match mode {
                OutputMode::Json => emit_json(
                    &SimpleEvent::PeerReturn {
                        nickname: nickname.as_str(),
                        pubkey: key.to_string().as_str(),
                        display: peer_return_display(nickname.as_str()),
                    },
                    true,
                ),
                OutputMode::Silent => {}
            },
        );
    }

    /// A receipt is proof for the sender, not work for the agent, so it is
    /// pollable but never wakes the bell.
    pub(crate) fn receipt(&self, params: ReceiptParams<'_>) {
        let ReceiptParams {
            from,
            ref_id,
            state,
            ts,
        } = params;
        self.dispatch(
            || OutputEvent::Receipt {
                from: from.clone(),
                ref_id: ref_id.to_owned(),
                state: state.to_owned(),
                ts,
            },
            |mode| match mode {
                OutputMode::Json => emit_json(
                    &SimpleEvent::Receipt {
                        from: from.as_str(),
                        ref_id,
                        state,
                        ts,
                    },
                    false,
                ),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print an informational line.
    pub(crate) fn info(&self, message: &str) {
        self.dispatch(
            || OutputEvent::Info {
                message: message.to_owned(),
            },
            |mode| match mode {
                OutputMode::Json => emit_json(&SimpleEvent::Info { message }, false),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print an error line.
    pub(crate) fn error(&self, message: &str) {
        if let Some(nickname) = nickname_held_key(message) {
            let nickname = nickname.to_owned();
            self.dispatch(
                || OutputEvent::NicknameHeld {
                    nickname: nickname.clone(),
                },
                |_| {},
            );
        }
        self.dispatch(
            || OutputEvent::Error {
                message: message.to_owned(),
            },
            |mode| match mode {
                OutputMode::Json => emit_json(&SimpleEvent::Error { message }, false),
                OutputMode::Silent => {}
            },
        );
    }

    /// Emit the result of an `agent-gossip ping` round: per-peer RTT, plus how
    /// many of the known peers responded (the responder count is just
    /// `peers.len()`). `known` is the current peer roster size.
    pub(crate) fn ping_report(&self, peers: Vec<PingPeer>, known: usize) {
        // Not routed through `dispatch`: `peers` is owned and each arm
        // consumes (or drops) it, so there is nothing to clone.
        match self {
            Output::Capture { tx, tap, .. } => {
                if let Some(tap) = tap {
                    tap.send(OutputEvent::PingReport {
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
                            peers: &peers,
                            known,
                        },
                        true,
                    ),
                    OutputMode::Silent => {}
                }
                // Mirror into the daemon's surfaced-events ring so `poll`
                // can return the same `ping_report` the stream just showed.
                if let Some(tap) = tap {
                    tap.send(OutputEvent::PingReport { peers, known });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use habilis_network::embed::{NodeEvent, NodeSink};
    use habilis_network::protocol::{Channel, Identity, Message, MessageKind};
    use tokio::sync::mpsc::{Receiver, UnboundedReceiver, unbounded_channel};

    use super::{Output, SurfacedTap};
    use crate::a2a::a2a_app::{A2aApp, SurfacedIo};
    use crate::a2a::surfaced_event::SurfacedEvent;
    use crate::a2a::tuning::{SURFACED_EVENTS_CAP, SURFACED_TAP_CAP};
    use crate::emission::OutputEvent;

    /// A document of about 4 KB: one agent card, as a peer of the mesh writes it.
    fn card_document() -> serde_json::Value {
        serde_json::json!({ "peers": { "bob": { "card": "x".repeat(4096) } } })
    }

    /// The body of a Meta change that writes that card: the change of the document, about 4 KB.
    fn card_patch() -> String {
        serde_json::json!({ "k": "change", "c": "x".repeat(4096) }).to_string()
    }

    fn change(channel: Channel, kind: MessageKind) -> NodeEvent {
        NodeEvent::StateChanged {
            channel,
            event: Box::new(
                Message::fixture(kind, &card_patch())
                    .sign(&Identity::generate())
                    .authored(),
            ),
            document: card_document(),
            is_self: false,
        }
    }

    /// The bytes of the document that an event holds, as text. `Debug` reads the same through any
    /// wrapper of the document, and prints `None` for a document that the event does not hold.
    fn held_document_bytes(event: &OutputEvent) -> usize {
        let OutputEvent::StateChanged { document, .. } = event else {
            return 0;
        };
        format!("{document:?}").len()
    }

    fn drain(rx: &mut UnboundedReceiver<OutputEvent>) -> Vec<OutputEvent> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn drain_ring(rx: &mut Receiver<OutputEvent>) -> Vec<OutputEvent> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// A capturing output with a tap: the live stream is `live`, the ring feed is `ring`.
    fn tapped() -> (
        Output,
        UnboundedReceiver<OutputEvent>,
        Receiver<OutputEvent>,
    ) {
        let (live_tx, live) = unbounded_channel();
        let (ring_tap, ring) = SurfacedTap::channel(SURFACED_TAP_CAP);
        (Output::capture(live_tx).set_tap(ring_tap), live, ring)
    }

    /// The bytes of the change body that an event holds: the change itself, a patch.
    fn held_body_bytes(event: &OutputEvent) -> usize {
        let OutputEvent::StateChanged { event, .. } = event else {
            return 0;
        };
        event.body.as_str().len()
    }

    /// What the real surfaced ring holds after `count` changes went through the tap and
    /// `A2aApp::drain_surfaced_ring`, as the event loop does it.
    fn ring_after(count: usize, channel: Channel, kind: &MessageKind) -> Vec<SurfacedEvent> {
        let (live_tx, _live) = unbounded_channel();
        let mut app = A2aApp::new_with_io(SurfacedIo::new(Output::capture(live_tx)));
        for turn in 0..count {
            app.output.emit(change(channel, kind.clone()));
            // The loop drains the tap at each turn, so a burst never passes the tap cap here.
            if turn % SURFACED_TAP_CAP == 0 {
                app.drain_surfaced_ring();
            }
        }
        app.drain_surfaced_ring();
        app.surfaced.poll_since(None).events
    }

    /// The ring keeps up to `SURFACED_EVENTS_CAP` events. A Meta document is the agent card of
    /// every peer, and the poll JSON drops it, so the ring copy of a Meta change holds none: the
    /// document bytes that the ring holds for Meta changes are 0 per event. The event still holds
    /// its body, the change with the card (about 4 KB), so the ring holds `SURFACED_EVENTS_CAP`
    /// bodies. That bound is in the audit file, row R12.
    #[test]
    fn the_ring_holds_no_meta_document_after_ten_times_its_cap() {
        let events = ring_after(10 * SURFACED_EVENTS_CAP, Channel::Meta, &MessageKind::Meta);
        assert_eq!(
            events.len(),
            SURFACED_EVENTS_CAP,
            "the ring is full and capped"
        );
        let documents: usize = events
            .iter()
            .map(|item| held_document_bytes(&item.event))
            .sum();
        assert!(
            documents <= SURFACED_EVENTS_CAP * "None".len(),
            "the ring holds {documents} document bytes for {} Meta changes",
            events.len()
        );
        let bodies: usize = events.iter().map(|item| held_body_bytes(&item.event)).sum();
        assert_eq!(
            bodies,
            SURFACED_EVENTS_CAP * card_patch().len(),
            "the ring still holds one patch per event: the bound of the audit row R12"
        );
    }

    /// The ring never keeps an `Info` or an `Error`, so they must not take a place in the tap or
    /// count as lost when the tap is full.
    #[test]
    fn a_burst_of_events_the_ring_never_keeps_does_not_fill_the_tap_or_count_as_lost() {
        let (live_tx, _live) = unbounded_channel();
        let (ring_tap, mut ring) = SurfacedTap::channel(SURFACED_TAP_CAP);
        let output = Output::capture(live_tx).set_tap(ring_tap.clone());
        for index in 0..10 * SURFACED_TAP_CAP {
            output.info(&format!("beat {index}"));
            output.error(&format!("error {index}"));
        }

        assert_eq!(
            drain_ring(&mut ring).len(),
            0,
            "the tap holds nothing the ring would not keep"
        );
        assert_eq!(ring_tap.take_lost(), 0, "no loss is counted for them");
    }

    /// The live stream is what an in-process reader sees, so it keeps the whole document.
    #[test]
    fn the_live_copy_of_a_meta_change_still_carries_the_document() {
        let (output, mut live, _ring) = tapped();
        output.emit(change(Channel::Meta, MessageKind::Meta));
        let events = drain(&mut live);
        assert_eq!(events.len(), 1, "one live event");
        assert!(
            held_document_bytes(&events[0]) > 4096,
            "the live copy holds the card"
        );
    }

    /// A State document is part of the poll JSON, so its ring copy keeps the document.
    #[test]
    fn the_ring_keeps_the_document_of_a_state_change() {
        let events = ring_after(1, Channel::State, &MessageKind::State);
        assert_eq!(events.len(), 1, "one ring event");
        assert!(
            held_document_bytes(&events[0].event) > 4096,
            "the ring copy holds the document"
        );
    }

    /// The live copy and the ring copy of one State change share one document: no deep copy per
    /// copy of the event.
    #[test]
    fn the_copies_of_a_state_change_share_one_document() {
        let (output, mut live, mut ring) = tapped();
        output.emit(change(Channel::State, MessageKind::State));
        let (live, ring) = (drain(&mut live), drain_ring(&mut ring));
        let (
            [
                OutputEvent::StateChanged {
                    document: Some(live_document),
                    ..
                },
            ],
            [
                OutputEvent::StateChanged {
                    document: Some(ring_document),
                    ..
                },
            ],
        ) = (live.as_slice(), ring.as_slice())
        else {
            panic!("one live event and one ring event, both with a document");
        };
        assert!(
            Arc::ptr_eq(live_document, ring_document),
            "the live copy and the ring copy hold two documents, not one"
        );
    }
}

/// The nickname that the gate's error names as held by another member, if `line` is that error.
/// A caller matches the event on the stable key `nickname_held`, not on these words.
pub(crate) fn nickname_held_key(line: &str) -> Option<&str> {
    line.strip_prefix("The nickname ")?
        .split_once(" is held by another member")
        .map(|(nickname, _)| nickname)
}

#[cfg(test)]
mod nickname_held_tests {
    /// The gate's error names the held nickname with a stable key, so an agent can match the
    /// event on the key and not on the words.
    #[test]
    fn the_gate_error_maps_to_the_nickname_held_key() {
        let line = "The nickname calm-otter is held by another member with another key. The two nodes do not accept the frames of each other";
        assert_eq!(
            super::nickname_held_key(line),
            Some("calm-otter"),
            "the held nickname is read from the engine's gate error"
        );
        assert_eq!(
            super::nickname_held_key("some other error"),
            None,
            "other errors are not this event"
        );
    }

    /// The poll shows the event with a display line, like the other visible events.
    #[test]
    fn the_nickname_held_line_has_a_display_line() {
        let line = super::super::simple_event::event_json(&super::OutputEvent::NicknameHeld {
            nickname: "calm-otter".to_owned(),
        })
        .expect("the event has a JSON line");
        let value: serde_json::Value = serde_json::from_str(&line).expect("the line is JSON");
        assert!(
            value["display"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "the line carries a display text: {line}"
        );
    }
}
