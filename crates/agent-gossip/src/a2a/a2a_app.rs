use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use habilis_network::ops::blob::{BlobServer, ContentId};
use habilis_network::protocol::messages::{Authored, Verified, WireFrame};
use habilis_network::protocol::{AuthorKey, DedupKey, MeshId, Nickname, ShardGroup, UnixSecs};
use habilis_network::util::bounded_fifo_set::BoundedFifoSet;
use tokio::sync::mpsc;
use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::oneshot::{Receiver, Sender};
use tokio::time::Instant as TokioInstant;

use crate::a2a::a2a_binding::A2aBinding;
use crate::a2a::extensions::{
    META_PROGRESS, META_SEQ, META_STATUS_TEXT, META_STATUS_TRUNCATED, META_WAITING_USER,
};
use crate::a2a::repair_budget::RepairBudget;
use crate::a2a::rpc_error::{self, A2aRequest, RpcError};
use crate::a2a::status_update_params::{Progress, capped_text, clean_label};
use crate::a2a::surfaced_event::SurfacedState;
use crate::a2a::task_record::{ClosedTasks, TaskRecord};
use crate::a2a::{META_LABEL, TaskId, a2a_binding, broadcast_params, node, task_record, tuning};
use crate::emission::{Output, OutputEvent, SurfacedTap};

/// The tapped `Output` plus its surfaced-event receiver — the app's slice of the
/// daemon's surfacing plumbing, assembled by the caller (CLI / api / MCP) from
/// its base `Output` and handed to [`A2aApp::new_with_io`].
pub(crate) struct SurfacedIo {
    output: Output,
    surfaced_rx: mpsc::Receiver<OutputEvent>,
    tap: SurfacedTap,
    /// Startup diagnostics this product announces alongside the engine's bare
    /// `Ready`. Both are ours: a stale skill install, and the port *we* bound.
    startup: Startup,
}

/// What [`StartupSink`] splices into the engine's identity-only `Ready`.
#[derive(Debug, Default, Clone)]
pub(crate) struct Startup {
    pub(crate) drift: Option<String>,
    pub(crate) a2a_port: Option<u16>,
}

/// Renders `Ready` with this product's startup diagnostics attached, and passes
/// every other engine surfacing straight through.
///
/// The engine emits mesh identity and nothing else — it has no notion of a skill
/// install or of what we serve on localhost. It used to carry both purely to
/// hand them back, which is why the `http_port` → `a2a_port` rename happened on
/// the way out.
struct StartupSink {
    output: Output,
    startup: Startup,
}

impl habilis_network::embed::NodeSink for StartupSink {
    fn emit(&self, event: habilis_network::embed::NodeEvent) {
        use habilis_network::embed::NodeEvent;
        if let NodeEvent::Ready {
            mesh,
            name,
            nickname,
        } = &event
        {
            self.output.ready(crate::emission::ReadyParams {
                mesh,
                name,
                nickname,
                drift: self.startup.drift.as_deref(),
                a2a_port: self.startup.a2a_port,
            });
            return;
        }
        self.output.emit(event);
    }
}

impl SurfacedIo {
    /// Tap `base` so every surfaced event is mirrored into the ring receiver.
    pub(crate) fn new(base: Output) -> Self {
        let (tap, rx) = SurfacedTap::channel(tuning::SURFACED_TAP_CAP);
        Self {
            output: base.set_tap(tap.clone()),
            surfaced_rx: rx,
            tap,
            startup: Startup::default(),
        }
    }

    /// Attach this product's startup diagnostics to the `ready` announcement.
    /// The in-process paths skip it — they compute no drift and bind no port.
    pub(crate) fn set_startup(mut self, startup: Startup) -> Self {
        self.startup = startup;
        self
    }

    /// The engine sink for this tapped `Output`: a clone shared with the app's
    /// renderer, wrapped as a [`NodeSink`](habilis_network::embed::NodeSink) so the engine emits `NodeEvent`s
    /// through the *same* tap the app's own `Output` writes to. Both feed the
    /// surfaced-events ring in surfacing order.
    pub(crate) fn sink(&self) -> impl habilis_network::embed::NodeSink + 'static {
        StartupSink {
            output: self.output.clone(),
            startup: self.startup.clone(),
        }
    }
}

/// All a2a-application state the event loop owns, split out of
/// `EventLoopState` so the mesh engine and the a2a layer keep disjoint state.
pub(crate) struct A2aApp {
    /// In-flight tasks this node is a party to, keyed by `task_id`
    /// (see [`crate::a2a::task_record`]). The coarse state machine + the two
    /// task timers (debounce sweep, keepalive) read/write this;
    /// the skill owns the content. Third-party relays never insert here.
    pub tasks: HashMap<TaskId, TaskRecord>,
    /// Tasks the sweep reaped after they closed, with their peer and last
    /// state. The record goes 2 min after close, but a relay can deliver a
    /// leg of it hours later, and the peer can still ask for the task.
    pub closed_tasks: ClosedTasks,
    /// `Message::dedup_key` of every chat line surfaced, the same
    /// `(pubkey, id)` key the engine dedups on. The engine forgets a key after
    /// a bounded count of frames, and a relay can then deliver the line again.
    pub surfaced_chat: BoundedFifoSet<DedupKey>,
    /// Outstanding gossip A2A RPC calls: an `A2aReq` was broadcast toward a
    /// peer and we're waiting for its `A2aResp` (matched by `rpc_id`) or the
    /// call's deadline. Fulfilled directly by the matching response frame.
    /// Bounded by [`POLL_WAITERS_CAP`](crate::a2a::tuning::POLL_WAITERS_CAP).
    pub a2a_waiters: Vec<A2aWaiter>,
    /// The blob channel's serving endpoint + content-addressed store, bound
    /// lazily on the first large-file offload (an `a2a artifact`/`call --file`)
    /// and kept for the process lifetime so its address stays stable while we're
    /// alive to serve. `None` until the first offload; closed on shutdown.
    pub blob_server: Option<BlobServer>,
    /// Blob groups of openings that never reached a worker, for the next task sweep to evict. A
    /// group is named by its task, whose record the call took back. Bounded by
    /// `ORPHAN_BLOBS_CAP`: past it the oldest is forgotten, and the caps of the blob store evict
    /// its blob.
    orphan_blobs: VecDeque<ContentId>,
    /// Ends owed to peers: a task that this node ended by a leg of its own, whose peer still beats
    /// as if it never got that leg. The receive path sends them after the frame that owed one, and
    /// when the list reaches `OWED_ENDS_CAP`, and the task sweep drains the whole Vec, so it holds
    /// one frame's pushes at most.
    owed_ends: Vec<(TaskId, AuthorKey)>,
    /// Tasks whose worker showed, by a beat, a leg that never arrived, with the key of the worker:
    /// the receive path asks each worker for its task after the frame that showed it, so the Vec
    /// holds one frame's pushes at most.
    to_ask: Vec<(TaskId, AuthorKey)>,
    /// The localhost A2A JSON-RPC binding's bound port + bearer token, set by
    /// [`serve_a2a`](Self::serve_a2a) when `--a2a-serve` is on. `None` (the
    /// default) means no local binding; the fields are written to the session
    /// state file (the local client's mode-600 discovery channel) at startup.
    a2a_port: Option<u16>,
    a2a_token: Option<String>,
    /// The a2a layer's concrete render sink (the tapped `Output`). Used for
    /// a2a-specific surfacings (`print_task` / `task_message` / `print_message`
    /// / `task_timeout`) the engine's generic `NodeSink` doesn't cover; generic
    /// events reach the same tap through `ctx.sink`.
    pub(crate) output: Output,
    /// The surfaced-events ring + long-poll waiters (the `poll`/`fetch`
    /// history). Held here so the daemon engine never names its element type.
    pub(crate) surfaced: SurfacedState,
    /// Receiver end of the `Output` tap, drained into [`surfaced`](Self::surfaced)
    /// each loop iteration. `None` for a detached test `A2aApp` (no live loop).
    pub(crate) surfaced_rx: Option<mpsc::Receiver<OutputEvent>>,
    /// The sender end of the tap, kept to read how many events it dropped.
    surfaced_tap: Option<SurfacedTap>,
    /// Our own task legs in the order they were sent, waiting to enter the task record. A leg
    /// enters it when the peer got it, and never before an earlier leg has been settled: the
    /// record then holds only what the peer got, in the order the peer got it.
    own_legs: VecDeque<QueuedLeg>,
    /// Our own content legs whose send has no outcome yet. Each enters the dev log and the shard
    /// cache only if it was sent: a repair ask is served from the cache, so a leg whose send failed
    /// would otherwise reach the peer after the user was told it failed.
    pub(crate) unsent_legs: Vec<UnsentLeg>,
    /// The id and the addressee of each directed frame and task message we sent.
    /// Frame ids ride in cleartext headers, so any member can name one: a
    /// receipt counts only from the peer the frame went to.
    pub(crate) sent_refs: BoundedFifoSet<(String, AuthorKey)>,
    /// How many `shard/repair` asks we serve in a window, by member and in all. The table holds
    /// at most 32 keys and is cleared every 5 s, so it needs no other bound.
    pub(crate) repair_budget: RepairBudget,
    /// How many cached shard frames this node re-sent to requesters. Adversarial-suite only: it
    /// proves that a repair ask was served, whatever else delivers the body.
    #[cfg(feature = "adversarial")]
    pub(crate) shards_resent: usize,
    /// How many task legs that arrive are still to be dropped. Adversarial-suite only: it stands
    /// for a frame that the overlay loses after its sender counted it as sent.
    #[cfg(feature = "adversarial")]
    pub(crate) legs_to_drop: usize,
}

/// Own frames that enter the dev log and the shard cache once `sent` resolves to true.
pub(crate) struct UnsentLeg {
    pub(crate) frames: Vec<Verified>,
    /// A big shard group's wire frames, for the `shard/repair` cache.
    pub(crate) cache: Option<(ShardGroup, Vec<WireFrame>)>,
    pub(crate) sent: Receiver<bool>,
}

fn bytes_of(frames: &[WireFrame]) -> usize {
    frames.iter().map(WireFrame::len).sum()
}

impl UnsentLeg {
    fn held_bytes(&self) -> usize {
        let frames: usize = self.frames.iter().map(|frame| frame.wire_len()).sum();
        let cache: usize = self
            .cache
            .as_ref()
            .map_or(0, |(_, cached)| bytes_of(cached));
        frames + cache
    }
}

/// An own task leg to ingest into the task record once the peer got it.
pub(crate) struct DeliveredLeg {
    pub(crate) echo: Authored,
    pub(crate) task_id: TaskId,
}

struct QueuedLeg {
    leg: DeliveredLeg,
    /// Whether the send of the leg reached the peer. Resolved when the send has an outcome.
    delivered: Receiver<bool>,
}

impl A2aApp {
    /// Build a detached a2a-application state (a silent sink, no surfacing tap)
    /// — for unit tests that drive handlers without a live event loop.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_surfacing(Output::silent(), None)
    }

    /// Build the a2a-application state wired to a live surfacing tap: the tapped
    /// `Output` for a2a renders and the receiver drained into the ring.
    pub(crate) fn new_with_io(io: SurfacedIo) -> Self {
        let SurfacedIo {
            output,
            surfaced_rx,
            tap,
            // Consumed by `sink()`, which the caller already built.
            startup: _,
        } = io;
        Self::with_surfacing(output, Some((surfaced_rx, tap)))
    }

    /// `tap` is the receiver and the sender of the surfacing tap, `None` for a detached test
    /// `A2aApp` that has no live loop to drain it.
    fn with_surfacing(
        output: Output,
        tap: Option<(mpsc::Receiver<OutputEvent>, SurfacedTap)>,
    ) -> Self {
        let (surfaced_rx, surfaced_tap) = tap.unzip();
        Self {
            tasks: HashMap::new(),
            closed_tasks: ClosedTasks::new(tuning::TASKS_CAP),
            surfaced_chat: BoundedFifoSet::new(tuning::CHAT_SURFACED_IDS_CAP),
            a2a_waiters: Vec::new(),
            blob_server: None,
            orphan_blobs: VecDeque::new(),
            owed_ends: Vec::new(),
            to_ask: Vec::new(),
            a2a_port: None,
            a2a_token: None,
            output,
            surfaced: SurfacedState::new(),
            surfaced_rx,
            surfaced_tap,
            own_legs: VecDeque::new(),
            unsent_legs: Vec::new(),
            sent_refs: BoundedFifoSet::new(tuning::CHAT_SURFACED_IDS_CAP),
            repair_budget: RepairBudget::new(Instant::now()),
            #[cfg(feature = "adversarial")]
            shards_resent: 0,
            #[cfg(feature = "adversarial")]
            legs_to_drop: 0,
        }
    }

    /// Say whether a new own task leg of `bytes` may wait for its send. Refuses past
    /// [`tuning::OWN_LEGS_CAP`] legs or [`tuning::OWN_LEGS_BYTES_CAP`] bytes.
    pub(crate) fn admit_own_leg(&self, bytes: usize) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.own_legs.len() < tuning::own_legs_cap(),
            "{} own task legs still wait for their send (limit {}), so this leg was not sent",
            self.own_legs.len(),
            tuning::own_legs_cap()
        );
        self.admit_own_bytes_within(bytes, tuning::own_legs_bytes_cap())
    }

    /// Say whether a new own content leg of `bytes` may wait for its send. Refuses past
    /// [`tuning::UNSENT_LEGS_CAP`] legs or [`tuning::UNSENT_LEGS_BYTES_CAP`] bytes.
    pub(crate) fn admit_unsent_leg(&self, bytes: usize) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.unsent_legs.len() < tuning::unsent_legs_cap(),
            "{} own content legs still wait for their send (limit {}), so this leg was not sent",
            self.unsent_legs.len(),
            tuning::unsent_legs_cap()
        );
        self.admit_unsent_bytes_within(bytes, tuning::unsent_legs_bytes_cap())
    }

    /// Count `count` cached shard frames that this node re-sent to a requester.
    #[cfg(feature = "adversarial")]
    pub(crate) fn note_shards_resent(&mut self, count: usize) {
        self.shards_resent += count;
    }

    /// The bytes of body that the own legs in the queue hold.
    pub(crate) fn own_legs_held_bytes(&self) -> usize {
        let lengths = self
            .own_legs
            .iter()
            .map(|queued| queued.leg.echo.body.as_str().len());
        lengths.sum()
    }

    /// The bytes that the own content legs in the list hold: the sealed frames and the shard cache.
    pub(crate) fn unsent_legs_held_bytes(&self) -> usize {
        self.unsent_legs.iter().map(UnsentLeg::held_bytes).sum()
    }

    fn admit_own_bytes_within(&self, bytes: usize, budget: usize) -> anyhow::Result<()> {
        let held = self.own_legs_held_bytes();
        anyhow::ensure!(
            self.own_legs.is_empty() || held.saturating_add(bytes) <= budget,
            "{held} bytes of own task legs still wait for their send, the budget is {budget}, so this leg of {bytes} bytes was not sent"
        );
        Ok(())
    }

    fn admit_unsent_bytes_within(&self, bytes: usize, budget: usize) -> anyhow::Result<()> {
        let held = self.unsent_legs_held_bytes();
        anyhow::ensure!(
            self.unsent_legs.is_empty() || held.saturating_add(bytes) <= budget,
            "{held} bytes of own content legs still wait for their send, the budget is {budget}, so this leg of {bytes} bytes was not sent"
        );
        Ok(())
    }

    /// Queue an own task leg behind the legs sent before it. `delivered` resolves to whether the
    /// peer got the leg.
    pub(crate) fn queue_own_leg(&mut self, leg: DeliveredLeg, delivered: Receiver<bool>) {
        self.own_legs.push_back(QueuedLeg { leg, delivered });
    }

    /// The legs at the head of the queue that the peer got, in the order they were sent. A leg
    /// whose send has no outcome yet holds back every leg behind it. A leg that was not delivered
    /// is dropped.
    fn take_delivered_legs(&mut self) -> Vec<DeliveredLeg> {
        let mut ready = Vec::new();
        while let Some(head) = self.own_legs.front_mut() {
            let delivered = match head.delivered.try_recv() {
                Ok(delivered) => delivered,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => false,
            };
            let Some(QueuedLeg { leg, .. }) = self.own_legs.pop_front() else {
                break;
            };
            if delivered {
                ready.push(leg);
            }
        }
        ready
    }

    /// Remember that `ref_id` went to `to`, so its receipts can be checked.
    pub(crate) fn note_sent_ref(&mut self, ref_id: &str, to: &AuthorKey) {
        self.sent_refs.insert((ref_id.to_owned(), *to));
    }

    /// Ingest the own task legs that the peer got since the last call.
    pub(crate) fn drain_delivered_legs(&mut self) {
        for DeliveredLeg { echo, task_id } in self.take_delivered_legs() {
            broadcast_params::ingest_own_leg(self, &echo, &task_id);
        }
    }

    /// Drain the `Output` tap into the ring, keeping only pollable events, then
    /// fulfill any long-poll waiter the new events advanced past.
    pub(crate) fn drain_surfaced_ring(&mut self) {
        let mut drained = Vec::new();
        if let Some(rx) = self.surfaced_rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                drained.push(event);
            }
        }
        for event in drained {
            if node::is_pollable(&event) {
                self.surfaced.push(event);
            }
        }
        let lost = self.surfaced_tap.as_ref().map_or(0, SurfacedTap::take_lost);
        if lost > 0 {
            self.surfaced.push(OutputEvent::EventsLost { lost });
        }
        self.surfaced.fulfill_ready_poll_waiters();
    }

    /// Take ownership of an already-bound localhost A2A binding: record its
    /// port + bearer token (for the state file) and spawn the HTTP task,
    /// returning the request channel the event loop services. Called by the
    /// CLI when `--a2a-serve` is set.
    pub(crate) fn serve_a2a(&mut self, binding: A2aBinding) -> mpsc::Receiver<A2aRequest> {
        self.a2a_port = Some(binding.port);
        self.a2a_token = Some(binding.token.clone());
        let (tx, rx) = mpsc::channel::<A2aRequest>(a2a_binding::REQUEST_QUEUE);
        a2a_binding::spawn(binding, tx);
        rx
    }

    /// The a2a bind port + bearer token, if the local binding is on.
    pub(crate) fn a2a_discovery(&self) -> Option<(u16, &str)> {
        self.a2a_port.zip(self.a2a_token.as_deref())
    }

    /// The bound a2a port (`0` when no local binding), for gossip-RPC ops that
    /// echo it.
    pub(crate) fn a2a_port(&self) -> u16 {
        self.a2a_port.unwrap_or_default()
    }

    /// Register an outstanding gossip A2A call. Returns the responder back
    /// (unregistered) when the registry is at `POLL_WAITERS_CAP`, so the
    /// caller can fail it fast instead of parking silently.
    #[must_use]
    pub(crate) fn register_a2a_waiter(&mut self, waiter: A2aWaiter) -> Option<A2aResponder> {
        if self.a2a_waiters.len() >= tuning::POLL_WAITERS_CAP {
            return Some(waiter.responder);
        }
        self.a2a_waiters.push(waiter);
        None
    }

    /// Take back the record that an opening call made at dispatch, because the worker was never
    /// reached or refused the task. The file of the call, held under the task id, goes to the next
    /// sweep.
    pub(crate) fn drop_opening(&mut self, opening: Option<&Opening>) {
        let Some(Opening { task_id, file }) = opening else {
            return;
        };
        self.tasks.remove(task_id);
        if *file {
            self.orphan_blob(ContentId::new(task_id.as_str()));
        }
    }

    /// Take the parked call that `corr` names, so the caller can answer it now.
    pub(crate) fn take_a2a_waiter(
        &mut self,
        corr: &habilis_network::protocol::CorrId,
    ) -> Option<A2aWaiter> {
        let index = self
            .a2a_waiters
            .iter()
            .position(|waiter| waiter.corr == *corr)?;
        Some(self.a2a_waiters.swap_remove(index))
    }

    /// Resolve the waiter matching `corr` **from the peer we called** (`from_key`) with its
    /// JSON-RPC response `body`, and drop it. A response with no matching waiter, or one
    /// purporting to answer a call we made to a *different* peer (a forged reply with a guessed
    /// `corr`), is a no-op, and so is a reply that comes after the deadline: the waiter is gone.
    ///
    /// The `Task` that the response returns is reconciled with the record we hold. An error reply
    /// to an opening takes the record of the opening back.
    pub(crate) fn fulfill_a2a_waiter(
        &mut self,
        corr: &habilis_network::protocol::CorrId,
        from: (&AuthorKey, UnixSecs),
        body: &str,
    ) {
        let (from_key, sent_at) = from;
        let Some(index) = self
            .a2a_waiters
            .iter()
            .position(|waiter| waiter.corr == *corr && waiter.peer_key == *from_key)
        else {
            return;
        };
        let waiter = self.a2a_waiters.swap_remove(index);
        // An opening is kept only by a reply that names its task: an error, a reply that does
        // not parse and a reply about another task leave nothing that this node can follow.
        let answered = waiter.opening.as_ref().is_none_or(|opening| {
            !is_error_reply(body)
                && returned_task(body).is_some_and(|task| task.id == opening.task_id)
        });
        if answered {
            self.reconcile_returned_task((from_key, sent_at), body);
        } else {
            self.drop_opening(waiter.opening.as_ref());
        }
        waiter.responder.send_response(body);
    }

    /// Queue the blob group `group` of a call that never reached a worker for the next sweep.
    fn orphan_blob(&mut self, group: ContentId) {
        if self.orphan_blobs.len() >= tuning::ORPHAN_BLOBS_CAP {
            self.orphan_blobs.pop_front();
            tracing::warn!(
                "an orphan blob group was forgotten: its blob waits for the blob store caps"
            );
        }
        self.orphan_blobs.push_back(group);
    }

    /// Whether we have an outstanding A2A call to `peer` correlated by `corr` — the gate that
    /// keeps an unsolicited/forged response from being acted on.
    pub(crate) fn has_a2a_waiter(
        &self,
        corr: &habilis_network::protocol::CorrId,
        peer_key: &AuthorKey,
    ) -> bool {
        self.a2a_waiters
            .iter()
            .any(|waiter| waiter.corr == *corr && waiter.peer_key == *peer_key)
    }

    /// Owe `peer_key` the end of `task_id`. A task that is already owed is not listed twice.
    pub(crate) fn owe_end(&mut self, task_id: &TaskId, peer_key: &AuthorKey) {
        if !self.owed_ends.iter().any(|(owed, _)| owed == task_id) {
            self.owed_ends.push((task_id.clone(), *peer_key));
        }
    }

    /// Whether the ends owed to peers reached [`tuning::OWED_ENDS_CAP`]: the caller sends them at once.
    pub(crate) fn owed_ends_full(&self) -> bool {
        self.owed_ends.len() >= tuning::OWED_ENDS_CAP
    }

    /// Whether any end is owed.
    pub(crate) fn owes_ends(&self) -> bool {
        !self.owed_ends.is_empty()
    }

    pub(crate) fn take_owed_ends(&mut self) -> Vec<(TaskId, AuthorKey)> {
        std::mem::take(&mut self.owed_ends)
    }

    /// Ask the worker `peer_key` for `task_id` after this frame.
    pub(crate) fn ask_for_task(&mut self, task_id: &TaskId, peer_key: &AuthorKey) {
        self.to_ask.push((task_id.clone(), *peer_key));
    }

    /// The tasks that the receive path asks their workers about now.
    pub(crate) fn take_to_ask(&mut self) -> Vec<(TaskId, AuthorKey)> {
        std::mem::take(&mut self.to_ask)
    }

    /// The blob groups the task sweep must evict: the groups of openings that never reached a
    /// worker. The groups of the tasks it reaps are named by the tasks, and the sweep evicts them
    /// by id.
    pub(crate) fn take_orphan_blobs(&mut self) -> Vec<ContentId> {
        Vec::from(std::mem::take(&mut self.orphan_blobs))
    }

    /// Reconcile the `Task` that a reply returned (a `SendMessage` result is `{"task":…}`, a
    /// `GetTask` result is the bare `Task`) with the record that this node holds. A reply that
    /// returns no task, or a task we hold no record of, changes nothing: only dispatch opens a
    /// record on this side.
    fn reconcile_returned_task(&mut self, from: (&AuthorKey, UnixSecs), body: &str) {
        let (peer_key, sent_at) = from;
        let Some(returned) = returned_task(body) else {
            return;
        };
        let gap = task_record::reconcile_initiator(
            &mut self.tasks,
            task_record::ReconcileParams {
                task_id: &returned.id,
                peer_key,
                snapshot: returned.snapshot,
                sent_at,
                now: Instant::now(),
            },
        );
        // The worker echoes the label of the brief, so the alarm can name the task.
        task_record::set_label(&mut self.tasks, &returned.id, peer_key, returned.label);
        // A question or an end that was lost on the way: the agent of the sender must hear it.
        if let Some(gap) = gap {
            self.output.task_gap(&gap);
        }
    }

    /// The earliest A2A-call deadline, for the loop's `sleep_until_opt` arm.
    pub(crate) fn earliest_a2a_deadline(&self) -> Option<TokioInstant> {
        self.a2a_waiters.iter().map(|waiter| waiter.deadline).min()
    }

    /// Time out every A2A call past its deadline. An opening answers with its task, marked
    /// unconfirmed, because the record exists from dispatch and the worker may hold the task: the
    /// record stays, and the sweep cancels it if no leg of the worker arrives. Any other call
    /// answers with the timeout error.
    pub(crate) fn expire_a2a_waiters(&mut self, now: TokioInstant) {
        let (expired, survivors): (Vec<A2aWaiter>, Vec<A2aWaiter>) =
            std::mem::take(&mut self.a2a_waiters)
                .into_iter()
                .partition(|waiter| waiter.deadline <= now);
        self.a2a_waiters = survivors;
        for waiter in expired {
            match self.unconfirmed_reply(&waiter) {
                Some(body) => waiter.responder.send_response(&body),
                None => waiter.responder.send_timeout(),
            }
        }
    }

    /// The reply of an opening that timed out, when its record still exists.
    fn unconfirmed_reply(&self, waiter: &A2aWaiter) -> Option<String> {
        let task_id = &waiter.opening.as_ref()?.task_id;
        let rec = self.tasks.get(task_id)?;
        let task = rpc_error::unconfirmed_task(task_id, rec, &waiter.mesh);
        Some(serde_json::json!({ "result": { "task": task } }).to_string())
    }

    /// Time out every outstanding A2A call on shutdown.
    pub(crate) fn close_a2a_waiters(&mut self) {
        for waiter in std::mem::take(&mut self.a2a_waiters) {
            waiter.responder.send_timeout();
        }
    }

    /// A peer left the mesh gracefully. Waiters first: an opening that waits on the peer takes its
    /// record back, so the caller who is told that the peer left never sees a `task_timeout` for a
    /// task id it never got. The tasks that remain are then ended.
    pub(crate) fn peer_left(&mut self, (peer_key, peer): (&AuthorKey, &Nickname)) {
        self.fail_a2a_waiters_for_peer((peer_key, peer));
        let out = self.output.clone();
        task_record::fail_tasks_for_departed_peer(&mut self.tasks, peer_key, &out);
    }

    /// Resolve every parked A2A call to `peer` with a peer-left error, now —
    /// the response can never arrive. The graceful-`Left` analogue of
    /// [`Self::expire_a2a_waiters`].
    pub(crate) fn fail_a2a_waiters_for_peer(&mut self, (peer_key, peer): (&AuthorKey, &Nickname)) {
        let survivors: Vec<A2aWaiter> = std::mem::take(&mut self.a2a_waiters)
            .into_iter()
            .filter_map(|waiter| {
                if waiter.peer_key == *peer_key {
                    self.drop_opening(waiter.opening.as_ref());
                    waiter.responder.send_peer_left(peer);
                    None
                } else {
                    Some(waiter)
                }
            })
            .collect();
        self.a2a_waiters = survivors;
    }
}

/// How a fulfilled/expired gossip A2A call's response is delivered, per
/// transport: the CLI/IPC path wants the JSON-RPC response string to print;
/// the in-process path wants the parsed response `Value`; the localhost
/// JSON-RPC binding wants the `Result<Value, RpcError>` its HTTP handler
/// returns (so an external A2A client's `message/send` to a peer is served
/// over the same request/response waiter — task creation over the compliant
/// transport).
pub(crate) enum A2aResponder {
    Ipc(Sender<String>),
    Typed(Sender<serde_json::Value>),
    Rpc(Sender<Result<serde_json::Value, RpcError>>),
}

impl A2aResponder {
    /// Deliver the peer's JSON-RPC response (`body` is the full
    /// `{"result"|"error"}` object string).
    pub(crate) fn send_response(self, body: &str) {
        match self {
            A2aResponder::Ipc(tx) => {
                let _ = tx.send(body.to_string());
            }
            A2aResponder::Typed(tx) => {
                let value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
                let _ = tx.send(value);
            }
            A2aResponder::Rpc(tx) => {
                let _ = tx.send(rpc_result_from_body(body));
            }
        }
    }

    /// Deliver a timeout (no response arrived before the deadline).
    pub(crate) fn send_timeout(self) {
        self.send_error("a2a request timed out (peer unreachable or slow)");
    }

    /// Deliver a peer-left error (the addressee broadcast a graceful `Left`,
    /// so no response can come).
    pub(crate) fn send_peer_left(self, peer: &Nickname) {
        self.send_error(&format!("a2a peer '{peer}' left the mesh"));
    }

    /// Fail the call with `-32000` and `message`, in each transport's shape.
    fn send_error(self, message: &str) {
        let error = || serde_json::json!({ "error": { "code": -32000, "message": message } });
        match self {
            A2aResponder::Ipc(tx) => {
                let _ = tx.send(error().to_string());
            }
            A2aResponder::Typed(tx) => {
                let _ = tx.send(error());
            }
            A2aResponder::Rpc(tx) => {
                let _ = tx.send(Err(RpcError {
                    code: -32000,
                    message: message.to_string(),
                }));
            }
        }
    }
}

/// The `Task` that a reply returned: its id, what it says about the task, and the label that the
/// worker echoes.
struct ReturnedTask {
    id: TaskId,
    snapshot: task_record::Snapshot,
    label: Option<String>,
}

/// The `Task` of a JSON-RPC response `body`. A `SendMessage` result is `{"task":…}`, and a
/// `GetTask` result is the bare `Task`; a v1.0 Task has no inline `kind`, so it is recognized by
/// its required `id` and `status.state`. `None` for a body that is no response or holds no task.
fn returned_task(body: &str) -> Option<ReturnedTask> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let result = &value["result"];
    let task = if result.get("task").is_some() {
        &result["task"]
    } else {
        result
    };
    let id = task["id"].as_str().and_then(TaskId::from_uuid_str)?;
    let state = task["status"]["state"]
        .as_str()
        .and_then(|raw| serde_json::from_value(serde_json::Value::String(raw.to_owned())).ok())?;
    let meta = &task["metadata"];
    let mut text = meta[META_STATUS_TEXT]
        .as_str()
        .and_then(|text| capped_text(text, tuning::TASK_STATUS_TEXT_MAX_CHARS));
    if let Some(capped) = text.as_mut() {
        capped.truncated |= meta[META_STATUS_TRUNCATED].as_bool().unwrap_or(false);
    }
    let waiting_user = meta[META_WAITING_USER].as_bool().unwrap_or(false);
    let review = meta["mesh:review"].as_bool().unwrap_or(false);
    let snapshot = task_record::Snapshot {
        state,
        seq: meta[META_SEQ].as_u64(),
        progress: progress_of(meta),
        text,
        waiting_user,
        review,
    };
    let label = meta[META_LABEL].as_str().and_then(clean_label);
    Some(ReturnedTask {
        id,
        snapshot,
        label,
    })
}

/// The progress that a task's metadata states: a whole percentage from 0 to 100.
fn progress_of(meta: &serde_json::Value) -> Option<Progress> {
    let percent = u8::try_from(meta[META_PROGRESS].as_u64()?).ok()?;
    Progress::try_from(percent).ok()
}

/// Whether a peer's JSON-RPC response `body` is an error.
fn is_error_reply(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body).is_ok_and(|value| !value["error"].is_null())
}

/// Parse a peer's JSON-RPC response `body` (`{"result":…}` | `{"error":…}`)
/// into the `Result<Value, RpcError>` the localhost binding's HTTP handler
/// returns. A malformed/absent body reads as an internal error.
fn rpc_result_from_body(body: &str) -> Result<serde_json::Value, RpcError> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Err(RpcError::internal("peer returned a malformed a2a response"));
    };
    if !value["error"].is_null() {
        let code: i64 = value["error"]["code"].as_i64().unwrap_or(-32603);
        let message: String = value["error"]["message"]
            .as_str()
            .unwrap_or("peer error")
            .to_string();
        return Err(RpcError { code, message });
    }
    Ok(value["result"].clone())
}

/// The task that an opening call made at dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Opening {
    pub(crate) task_id: TaskId,
    /// Whether the call attached a file, which the blob group named by the task holds.
    pub(crate) file: bool,
}

/// An outstanding gossip A2A call, waiting for a response frame with a matching
/// correlation id `corr` from `peer`, or for `deadline` to elapse.
pub(crate) struct A2aWaiter {
    pub(crate) corr: habilis_network::protocol::CorrId,
    /// The key the call was sent to. A reply must be signed by this key: a nickname alone can be
    /// held by two members. Defence-in-depth: a reply without the random corr id is not forged.
    pub(crate) peer_key: AuthorKey,
    pub(crate) deadline: TokioInstant,
    pub(crate) responder: A2aResponder,
    /// The task this call opened, whose record exists from dispatch: an error reply takes it
    /// back. A follow-up opens none.
    pub(crate) opening: Option<Opening>,
    /// The mesh of the call, for the task that a timed-out opening answers with.
    pub(crate) mesh: MeshId,
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use habilis_network::ops::blob::ContentId;
    use habilis_network::protocol::{
        AuthorKey, MeshId, Message, MessageBody, MessageKind, Nickname, UnixSecs,
    };
    use tokio::sync::oneshot;

    use super::{A2aResponder, DeliveredLeg, rpc_result_from_body};
    use crate::a2a::task_record::{TaskRecord, TaskRole};
    use crate::a2a::{TaskId, TaskState, tuning, wire};
    use crate::emission::{Output, OutputEvent};

    fn leg(body: &str) -> DeliveredLeg {
        let echo = Message::new_frame(
            &MeshId::from("test"),
            &Nickname::from("alice"),
            MessageKind::App {
                tag: wire::app_tag("status"),
                to: None,
                corr: None,
            },
            MessageBody::from(body),
        );
        DeliveredLeg {
            echo: echo
                .sign(&habilis_network::protocol::Identity::generate())
                .authored(),
            task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
        }
    }

    #[test]
    fn the_tap_holds_at_most_its_cap_of_events() {
        let mut io = super::SurfacedIo::new(Output::silent());
        for _ in 0..10 * tuning::SURFACED_TAP_CAP {
            io.output.peer_return(&Nickname::from("alice"), &bob_key());
        }

        let mut queued = 0;
        while io.surfaced_rx.try_recv().is_ok() {
            queued += 1;
        }

        assert_eq!(
            queued,
            tuning::SURFACED_TAP_CAP,
            "10 times the cap was offered before one drain: the channel must not grow past the cap"
        );
    }

    #[test]
    fn the_ring_gets_one_marker_that_counts_the_events_the_tap_dropped() {
        let mut app = super::A2aApp::new_with_io(super::SurfacedIo::new(Output::silent()));
        for _ in 0..10 * tuning::SURFACED_TAP_CAP {
            app.output.peer_return(&Nickname::from("alice"), &bob_key());
        }

        app.drain_surfaced_ring();
        let batch = app.surfaced.poll_since(None);

        let newest = batch.events.last().map(|item| &item.event);
        assert!(
            matches!(
                newest,
                Some(OutputEvent::EventsLost { lost }) if *lost == 9 * tuning::SURFACED_TAP_CAP as u64
            ),
            "the newest ring entry must say how many events the tap dropped, got {newest:?}"
        );
    }

    #[test]
    fn orphan_blobs_stop_at_the_cap_and_drop_the_oldest() {
        let mut app = super::A2aApp::new();
        for index in 0..10 * tuning::ORPHAN_BLOBS_CAP {
            app.orphan_blob(ContentId::new(&format!("group-{index}")));
        }

        let kept: Vec<&ContentId> = app.orphan_blobs.iter().collect();
        let newest = ContentId::new(&format!("group-{}", 10 * tuning::ORPHAN_BLOBS_CAP - 1));
        assert_eq!(
            kept.len(),
            tuning::ORPHAN_BLOBS_CAP,
            "10 times the cap was offered: the list must not grow past the cap"
        );
        assert_eq!(
            kept.last().copied(),
            Some(&newest),
            "the newest orphan is kept, the oldest are the ones dropped"
        );
        assert!(
            !kept.contains(&&ContentId::new("group-0")),
            "the oldest orphan must be gone, or the cap drops the new ones"
        );
    }

    /// The receive path sends the owed ends at once when the list is full, so the list never holds
    /// more than the cap, and no end is dropped.
    #[test]
    fn the_owed_ends_never_pass_the_cap_and_none_is_lost() {
        let mut app = super::A2aApp::new();
        let mut sent = 0;
        let mut longest = 0;
        for _ in 0..10 * tuning::OWED_ENDS_CAP {
            app.owe_end(&TaskId::random(), &bob_key());
            longest = longest.max(app.owed_ends.len());
            if app.owed_ends_full() {
                sent += app.take_owed_ends().len();
            }
        }
        sent += app.take_owed_ends().len();

        assert_eq!(
            longest,
            tuning::OWED_ENDS_CAP,
            "10 times the cap was owed: the list must not hold more than the cap"
        );
        assert_eq!(
            sent,
            10 * tuning::OWED_ENDS_CAP,
            "every owed end must be sent, none dropped for want of room"
        );
    }

    /// A task is owed once, however many beats of the peer ask for its end.
    #[test]
    fn a_task_is_owed_its_end_once() {
        let mut app = super::A2aApp::new();
        let task_id = TaskId::random();
        for _ in 0..5 {
            app.owe_end(&task_id, &bob_key());
        }
        assert_eq!(app.take_owed_ends().len(), 1, "one beat or five, one end");
    }

    #[test]
    fn own_legs_stop_at_the_cap_and_the_rest_is_refused() {
        let mut app = super::A2aApp::new();
        let mut held = Vec::new();
        let mut refused = 0;
        for _ in 0..10 * tuning::OWN_LEGS_CAP {
            if app.admit_own_leg(0).is_err() {
                refused += 1;
                continue;
            }
            let (tx, rx) = oneshot::channel();
            held.push(tx);
            app.queue_own_leg(leg("status"), rx);
        }

        assert_eq!(
            app.own_legs.len(),
            tuning::OWN_LEGS_CAP,
            "10 times the cap was offered: the queue must not grow past the cap"
        );
        assert_eq!(
            refused,
            9 * tuning::OWN_LEGS_CAP,
            "a leg that finds the queue full must be refused, not queued"
        );
    }

    #[test]
    fn unsent_legs_stop_at_the_cap_and_the_rest_is_refused() {
        let mut app = super::A2aApp::new();
        let mut held = Vec::new();
        let mut refused = 0;
        for _ in 0..10 * tuning::UNSENT_LEGS_CAP {
            if app.admit_unsent_leg(0).is_err() {
                refused += 1;
                continue;
            }
            let (tx, sent) = oneshot::channel();
            held.push(tx);
            app.unsent_legs.push(super::UnsentLeg {
                frames: Vec::new(),
                cache: None,
                sent,
            });
        }

        assert_eq!(
            app.unsent_legs.len(),
            tuning::UNSENT_LEGS_CAP,
            "10 times the cap was offered: the list must not grow past the cap"
        );
        assert_eq!(
            refused,
            9 * tuning::UNSENT_LEGS_CAP,
            "a leg that finds the queue full must be refused, not queued"
        );
    }

    const MIB: usize = 1024 * 1024;

    #[test]
    fn own_legs_stop_at_the_byte_budget_and_the_rest_is_refused() {
        let budget = 16 * MIB;
        let mut app = super::A2aApp::new();
        let mut held = Vec::new();
        let mut refused = 0;
        for _ in 0..10 * budget / MIB {
            if app.admit_own_bytes_within(MIB, budget).is_err() {
                refused += 1;
                continue;
            }
            let (tx, rx) = oneshot::channel();
            held.push(tx);
            app.queue_own_leg(leg(&"x".repeat(MIB)), rx);
        }

        assert_eq!(
            app.own_legs_held_bytes(),
            budget,
            "10 times the budget was offered: the held bytes must not pass the budget"
        );
        assert_eq!(
            refused,
            9 * budget / MIB,
            "a leg that would pass the budget must be refused, not queued"
        );
    }

    #[test]
    fn one_own_leg_always_fits_even_past_the_budget() {
        let app = super::A2aApp::new();

        assert!(
            app.admit_own_bytes_within(2 * MIB, MIB).is_ok(),
            "the largest legal body must not be refused for its size when nothing waits"
        );
    }

    #[test]
    fn unsent_legs_stop_at_the_byte_budget_and_the_rest_is_refused() {
        let frame = Message::new_frame(
            &MeshId::from("test"),
            &Nickname::from("alice"),
            MessageKind::App {
                tag: wire::app_tag("status"),
                to: None,
                corr: None,
            },
            MessageBody::from("x"),
        )
        .sign(&habilis_network::protocol::Identity::generate());
        let size = frame.wire_len();
        let budget = 64 * size;
        let mut app = super::A2aApp::new();
        let mut held = Vec::new();
        let mut refused = 0;
        for _ in 0..10 * 64 {
            if app.admit_unsent_bytes_within(size, budget).is_err() {
                refused += 1;
                continue;
            }
            let (tx, sent) = oneshot::channel();
            held.push(tx);
            app.unsent_legs.push(super::UnsentLeg {
                frames: vec![frame.clone()],
                cache: None,
                sent,
            });
        }

        assert_eq!(
            app.unsent_legs_held_bytes(),
            budget,
            "10 times the budget was offered: the held bytes must not pass the budget"
        );
        assert_eq!(
            refused,
            9 * 64,
            "a leg that would pass the budget must be refused, not queued"
        );
    }

    fn bodies(legs: &[DeliveredLeg]) -> Vec<&str> {
        legs.iter().map(|leg| leg.echo.body.as_str()).collect()
    }

    /// Two legs go to one cold peer and share its dial, so the verdict of the second can come
    /// first. The record must still take the status before the artifact: the task ends in the
    /// state of the last leg.
    #[test]
    fn own_legs_enter_the_record_in_send_order_when_a_later_verdict_comes_first() {
        let mut app = super::A2aApp::new();
        let (status_tx, status_rx) = oneshot::channel();
        let (artifact_tx, artifact_rx) = oneshot::channel();
        app.queue_own_leg(leg("status"), status_rx);
        app.queue_own_leg(leg("artifact"), artifact_rx);

        artifact_tx
            .send(true)
            .expect("the queue holds the receiver");
        assert!(
            app.take_delivered_legs().is_empty(),
            "the artifact waits behind the status, whose send has no outcome"
        );

        status_tx.send(true).expect("the queue holds the receiver");
        let taken = app.take_delivered_legs();
        assert_eq!(
            bodies(&taken),
            ["status", "artifact"],
            "both legs are taken in order"
        );
        assert!(app.take_delivered_legs().is_empty(), "a leg enters once");
    }

    /// A leg that the dial did not send leaves no record, and does not hold back the next one.
    #[test]
    fn a_leg_that_was_not_delivered_leaves_no_record_and_does_not_block_the_next() {
        let mut app = super::A2aApp::new();
        let (status_tx, status_rx) = oneshot::channel();
        let (artifact_tx, artifact_rx) = oneshot::channel();
        app.queue_own_leg(leg("status"), status_rx);
        app.queue_own_leg(leg("artifact"), artifact_rx);

        status_tx.send(false).expect("the queue holds the receiver");
        artifact_tx
            .send(true)
            .expect("the queue holds the receiver");
        let taken = app.take_delivered_legs();
        assert_eq!(
            bodies(&taken),
            ["artifact"],
            "only the artifact is taken after the cancel"
        );
    }

    /// A send whose task ended without an outcome is a failed send.
    #[test]
    fn a_leg_whose_send_task_was_dropped_is_not_delivered() {
        let mut app = super::A2aApp::new();
        let (status_tx, status_rx) = oneshot::channel::<bool>();
        app.queue_own_leg(leg("status"), status_rx);
        drop(status_tx);
        assert!(
            app.take_delivered_legs().is_empty(),
            "delivered legs are taken once"
        );
        assert!(app.own_legs.is_empty(), "the failed leg left the queue");
    }

    /// The localhost binding's `A2aResponder::Rpc` unwraps a peer's JSON-RPC
    /// response body into the `Result<Value, RpcError>` its HTTP handler
    /// returns: `result` → `Ok`, `error` → the peer's code/message, and a
    /// malformed/absent body → an internal error (never a panic).
    #[test]
    fn rpc_result_from_body_maps_result_error_and_garbage() {
        // A result comes back as Ok(the result value).
        let ok = rpc_result_from_body(r#"{"result":{"task":{"id":"t1"}}}"#)
            .expect("the body holds a result key and no error key, so the parse yields a value");
        assert_eq!(ok["task"]["id"], "t1", "an ok call returns the task id");

        // A JSON-RPC error carries the peer's code + message verbatim.
        let err = rpc_result_from_body(r#"{"error":{"code":-32602,"message":"unknown peer"}}"#)
            .expect_err("an error body is Err");
        assert_eq!(
            err.code, -32602,
            "an unknown peer is an invalid params error"
        );
        assert_eq!(
            err.message, "unknown peer",
            "the unknown peer error names the peer"
        );

        // Garbage is an internal error, not a panic.
        let garbage = rpc_result_from_body("not json").expect_err("garbage is Err");
        assert_eq!(
            garbage.code, -32603,
            "a garbage request is an internal error"
        );
    }

    /// The `Rpc` responder delivers a fulfilled peer response and a deadline
    /// timeout to the localhost HTTP handler's oneshot as `Result<Value,
    /// RpcError>`.
    #[test]
    fn rpc_responder_delivers_response_and_timeout() {
        // A fulfilled response resolves to the parsed result.
        let (ok_tx, ok_rx) = oneshot::channel();
        A2aResponder::Rpc(ok_tx).send_response(r#"{"result":{"task":{"id":"x"}}}"#);
        assert_eq!(
            ok_rx
                .blocking_recv()
                .expect("the sender replies before it is dropped")
                .expect("the test sends a fulfilled response")["task"]["id"],
            "x",
            "the call answers with the task id"
        );

        // A deadline timeout resolves to the standard -32000 error.
        let (timeout_tx, timeout_rx) = oneshot::channel();
        A2aResponder::Rpc(timeout_tx).send_timeout();
        assert_eq!(
            timeout_rx
                .blocking_recv()
                .expect("the sender replies before it is dropped")
                .expect_err("the test sends a timeout")
                .code,
            -32000,
            "the call is parked until the reply"
        );
    }

    /// A graceful `Left` resolves exactly the departed peer's parked calls —
    /// with a peer-left error, immediately — and leaves other peers' waiters
    /// parked for their own deadlines.
    #[test]
    fn peer_left_fails_only_that_peers_waiters() {
        let mut app = super::A2aApp::new();
        let far = tokio::time::Instant::now() + std::time::Duration::from_mins(5);
        let (bob_tx, bob_rx) = oneshot::channel();
        let (carol_tx, carol_rx) = oneshot::channel();
        app.a2a_waiters.push(super::A2aWaiter {
            corr: wire::corr_id("corr-bob"),
            peer_key: bob_key(),
            deadline: far,
            responder: A2aResponder::Typed(bob_tx),
            opening: None,
            mesh: MeshId::from("test"),
        });
        app.a2a_waiters.push(super::A2aWaiter {
            corr: wire::corr_id("corr-carol"),
            peer_key: carol_key(),
            deadline: far,
            responder: A2aResponder::Typed(carol_tx),
            opening: None,
            mesh: MeshId::from("test"),
        });

        app.fail_a2a_waiters_for_peer((&bob_key(), &Nickname::from("bob")));

        let bob_response = bob_rx.blocking_recv().expect("bob's call resolves now");
        assert_eq!(
            bob_response["error"]["code"], -32000,
            "bob receives the error code"
        );
        assert_eq!(
            bob_response["error"]["message"], "a2a peer 'bob' left the mesh",
            "the waiter is registered for the peer"
        );
        assert_eq!(app.a2a_waiters.len(), 1, "carol's waiter survives");
        assert_eq!(
            app.a2a_waiters[0].peer_key,
            carol_key(),
            "the waiter belongs to carol"
        );
        drop(app);
        assert!(
            carol_rx.blocking_recv().is_err(),
            "carol's waiter was never resolved, only dropped with the app"
        );
    }

    /// Two keys can use one nickname. When one of them leaves, only the calls sent to it fail.
    #[test]
    fn peer_left_fails_only_the_waiters_of_the_key_that_left() {
        let mut app = super::A2aApp::new();
        let far = tokio::time::Instant::now() + std::time::Duration::from_mins(5);
        let (left_tx, left_rx) = oneshot::channel();
        let (twin_tx, twin_rx) = oneshot::channel();
        for (name, key, tx) in [
            ("left", bob_key(), left_tx),
            ("twin", other_bob_key(), twin_tx),
        ] {
            app.a2a_waiters.push(super::A2aWaiter {
                corr: wire::corr_id(name),
                peer_key: key,
                deadline: far,
                responder: A2aResponder::Typed(tx),
                opening: None,
                mesh: MeshId::from("test"),
            });
        }

        app.fail_a2a_waiters_for_peer((&bob_key(), &Nickname::from("bob")));

        assert!(
            left_rx.blocking_recv().is_ok(),
            "the call to the key that left resolves now"
        );
        assert_eq!(
            app.a2a_waiters.len(),
            1,
            "the call to the twin key survives"
        );
        assert_eq!(
            app.a2a_waiters[0].peer_key,
            other_bob_key(),
            "the survivor is the call to the twin key"
        );
        drop(app);
        assert!(
            twin_rx.blocking_recv().is_err(),
            "the twin call was never resolved, only dropped with the app"
        );
    }

    /// Two members can share a nickname; only the key that the call was sent to may answer it.
    #[test]
    fn a_reply_from_another_key_with_the_same_name_does_not_fulfill_the_waiter() {
        let mut app = super::A2aApp::new();
        let (tx, mut rx) = oneshot::channel();
        app.a2a_waiters.push(super::A2aWaiter {
            corr: wire::corr_id("corr-key"),
            peer_key: bob_key(),
            deadline: tokio::time::Instant::now() + std::time::Duration::from_mins(5),
            responder: A2aResponder::Typed(tx),
            opening: None,
            mesh: MeshId::from("test"),
        });

        app.fulfill_a2a_waiter(
            &wire::corr_id("corr-key"),
            (&other_bob_key(), UnixSecs::now()),
            "{}",
        );

        assert!(
            rx.try_recv().is_err(),
            "a reply signed by another key that uses the name 'bob' must not resolve bob's call"
        );
        assert_eq!(app.a2a_waiters.len(), 1, "bob's call is still waiting");
    }

    fn carol_key() -> AuthorKey {
        AuthorKey::from([0xca; 32])
    }

    /// One of 64 keys, matching the 64 names of the filler peers: no key reaches its own quota.
    fn bob_key() -> AuthorKey {
        AuthorKey::from([0xb0; 32])
    }

    fn other_bob_key() -> AuthorKey {
        AuthorKey::from([0xb1; 32])
    }

    fn waiter(corr: &str) -> super::A2aWaiter {
        let (tx, _rx) = oneshot::channel();
        super::A2aWaiter {
            corr: wire::corr_id(corr),
            peer_key: bob_key(),
            deadline: tokio::time::Instant::now() + std::time::Duration::from_mins(5),
            responder: A2aResponder::Typed(tx),
            opening: None,
            mesh: MeshId::from("test"),
        }
    }

    /// The record that an opening call to bob makes at dispatch.
    fn dispatch_opening(app: &mut super::A2aApp, corr: &str) -> TaskId {
        let task_id = TaskId::random();
        app.tasks.insert(
            task_id.clone(),
            TaskRecord::open(
                (bob_key(), Nickname::from("bob")),
                TaskRole::Initiator,
                TaskState::Submitted,
                Instant::now(),
            ),
        );
        let mut parked = waiter(corr);
        parked.opening = Some(super::Opening {
            task_id: task_id.clone(),
            file: true,
        });
        app.a2a_waiters.push(parked);
        task_id
    }

    fn task_reply(task_id: &TaskId, state: &str) -> String {
        task_reply_at(task_id, state, 100)
    }

    /// The reply of a worker that holds `task_id` in `state` after `seq` legs.
    fn task_reply_at(task_id: &TaskId, state: &str, seq: u64) -> String {
        serde_json::json!({
            "result": { "task": {
                "id": task_id,
                "status": { "state": state },
                "metadata": { "mesh:seq": seq },
            } }
        })
        .to_string()
    }

    fn fulfill(app: &mut super::A2aApp, corr: &str, key: &AuthorKey, body: &str) {
        app.fulfill_a2a_waiter(&wire::corr_id(corr), (key, UnixSecs::now()), body);
    }

    /// The worker refused the opening: the record the call made at dispatch goes, and its file
    /// goes to the next sweep.
    #[test]
    fn an_error_reply_takes_back_the_record_of_an_opening() {
        let mut app = super::A2aApp::new();
        let task_id = dispatch_opening(&mut app, "corr-refused");
        let refusal = serde_json::json!({ "error": { "code": -32004, "message": "full" } });

        fulfill(&mut app, "corr-refused", &bob_key(), &refusal.to_string());

        assert!(
            !app.tasks.contains_key(&task_id),
            "a task that the worker refused is not ours to keep"
        );
        assert_eq!(
            app.take_orphan_blobs(),
            vec![ContentId::new(task_id.as_str())],
            "the file of a refused opening goes to the next sweep"
        );
    }

    /// A refused opening that attached no file has no group to release.
    #[test]
    fn a_refused_opening_without_a_file_releases_no_group() {
        let mut app = super::A2aApp::new();
        let task_id = dispatch_opening(&mut app, "corr-plain");
        app.a2a_waiters[0].opening = Some(super::Opening {
            task_id,
            file: false,
        });
        let refusal = serde_json::json!({ "error": { "code": -32004, "message": "full" } });

        fulfill(&mut app, "corr-plain", &bob_key(), &refusal.to_string());

        assert!(
            app.take_orphan_blobs().is_empty(),
            "no file went with the call, so no group waits for the sweep"
        );
    }

    /// A reply that does not name the task of the opening leaves nothing to follow: one that does
    /// not parse and one about another task take the record back.
    #[test]
    fn a_reply_that_does_not_name_the_task_takes_back_the_record_of_an_opening() {
        for body in [
            "not json".to_owned(),
            task_reply(&TaskId::random(), "TASK_STATE_SUBMITTED"),
        ] {
            let mut app = super::A2aApp::new();
            let task_id = dispatch_opening(&mut app, "corr-odd");

            fulfill(&mut app, "corr-odd", &bob_key(), &body);

            assert!(
                !app.tasks.contains_key(&task_id),
                "{body} leaves nothing that the caller can follow"
            );
        }
    }

    /// The snapshot that a `GetTask` returns moves a record whose legs were lost, and a lost
    /// question reaches the sender as a `task_gap` that carries the text of the question.
    #[test]
    fn a_snapshot_that_moves_the_task_into_a_question_raises_a_task_gap() {
        let mut app = super::A2aApp::new_with_io(super::SurfacedIo::new(Output::silent()));
        let task_id = dispatch_opening(&mut app, "corr-gap");
        let reply = serde_json::json!({
            "result": {
                "id": task_id,
                "status": { "state": "TASK_STATE_INPUT_REQUIRED" },
                "metadata": { "mesh:seq": 4, "mesh:status_text": "which port?" },
            }
        })
        .to_string();

        fulfill(&mut app, "corr-gap", &bob_key(), &reply);

        assert_eq!(
            app.tasks[&task_id].state,
            TaskState::InputRequired,
            "the snapshot moved the record to where the worker is"
        );
        app.drain_surfaced_ring();
        let gaps: Vec<Option<String>> = app
            .surfaced
            .poll_since(None)
            .events
            .iter()
            .filter_map(|item| match &item.event {
                OutputEvent::TaskGap { notice } => Some(notice.text.clone()),
                OutputEvent::NicknameHeld { .. }
                | OutputEvent::Ready { .. }
                | OutputEvent::Message { .. }
                | OutputEvent::Presence { .. }
                | OutputEvent::PeerTimeout { .. }
                | OutputEvent::PeerReturn { .. }
                | OutputEvent::Info { .. }
                | OutputEvent::EventsLost { .. }
                | OutputEvent::Error { .. }
                | OutputEvent::PingReport { .. }
                | OutputEvent::Task { .. }
                | OutputEvent::TaskMessage { .. }
                | OutputEvent::TaskTimeout { .. }
                | OutputEvent::TaskOverdue { .. }
                | OutputEvent::CheckinDue { .. }
                | OutputEvent::Receipt { .. }
                | OutputEvent::StateChanged { .. } => None,
            })
            .collect();
        assert_eq!(
            gaps,
            vec![Some("which port?".to_owned())],
            "the sender is told once, with the question"
        );
    }

    /// A worker that accepts the opening keeps the record, whatever state it reports.
    #[test]
    fn a_task_reply_keeps_the_record_of_an_opening() {
        let mut app = super::A2aApp::new();
        let task_id = dispatch_opening(&mut app, "corr-ok");

        fulfill(
            &mut app,
            "corr-ok",
            &bob_key(),
            &task_reply(&task_id, "TASK_STATE_SUBMITTED"),
        );

        assert_eq!(
            app.tasks[&task_id].state,
            TaskState::Submitted,
            "the record stays as the reply says"
        );
        assert!(app.take_orphan_blobs().is_empty(), "no group is released");
    }

    /// A call that timed out keeps its record: the worker may hold the task, and nothing cancels
    /// it before the sweep does.
    #[test]
    fn a_timed_out_opening_keeps_its_record_and_asks_for_no_cancel() {
        let mut app = super::A2aApp::new();
        let task_id = dispatch_opening(&mut app, "corr-slow");

        app.expire_a2a_waiters(tokio::time::Instant::now() + std::time::Duration::from_hours(1));

        assert!(
            app.tasks.contains_key(&task_id),
            "the record of a timed-out opening stays"
        );
        assert!(
            app.take_owed_ends().is_empty(),
            "no cancel is owed for a task that the worker may be running"
        );
        assert!(
            app.take_orphan_blobs().is_empty(),
            "the file stays with the task"
        );
        assert!(
            !app.has_a2a_waiter(&wire::corr_id("corr-slow"), &bob_key()),
            "the call is over: nothing awaits its reply"
        );
    }

    /// A call that opened a task gets the task back when its reply is late: the record exists
    /// from dispatch and the worker may hold the task, so the caller can follow it.
    #[test]
    fn a_timed_out_opening_answers_with_its_unconfirmed_task() {
        let mut app = super::A2aApp::new();
        let task_id = dispatch_opening(&mut app, "corr-slow");
        let (tx, mut rx) = oneshot::channel();
        app.a2a_waiters[0].responder = A2aResponder::Typed(tx);

        app.expire_a2a_waiters(tokio::time::Instant::now() + std::time::Duration::from_hours(1));

        let reply = rx.try_recv().expect("an expired call is answered at once");
        let task = &reply["result"]["task"];
        assert_eq!(
            task["id"],
            task_id.as_str(),
            "the reply names the task of the call"
        );
        assert_eq!(
            task["status"]["state"], "TASK_STATE_SUBMITTED",
            "nothing says that the worker accepted it"
        );
        assert_eq!(
            task["metadata"]["mesh:unconfirmed"], true,
            "the reply says that the worker has not confirmed the task"
        );
        assert!(
            reply["error"].is_null(),
            "the caller reads a task, not a failure"
        );
    }

    /// A call that opened no task has nothing to hand back: it times out as an error.
    #[test]
    fn a_timed_out_call_that_opened_no_task_answers_with_the_timeout_error() {
        let mut app = super::A2aApp::new();
        let (tx, mut rx) = oneshot::channel();
        let mut parked = waiter("corr-follow-up");
        parked.responder = A2aResponder::Typed(tx);
        app.a2a_waiters.push(parked);

        app.expire_a2a_waiters(tokio::time::Instant::now() + std::time::Duration::from_hours(1));

        let reply = rx.try_recv().expect("an expired call is answered at once");
        assert!(
            reply["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("timed out")),
            "a follow-up that gets no reply is a timeout, got {reply}"
        );
    }

    /// An opening whose record the sweep already took has no task to hand back.
    #[test]
    fn a_timed_out_opening_without_its_record_answers_with_the_timeout_error() {
        let mut app = super::A2aApp::new();
        let task_id = dispatch_opening(&mut app, "corr-gone");
        let (tx, mut rx) = oneshot::channel();
        app.a2a_waiters[0].responder = A2aResponder::Typed(tx);
        app.tasks.remove(&task_id);

        app.expire_a2a_waiters(tokio::time::Instant::now() + std::time::Duration::from_hours(1));

        let reply = rx.try_recv().expect("an expired call is answered at once");
        assert!(
            reply["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("timed out")),
            "a task that is gone cannot be handed back, got {reply}"
        );
    }

    /// A reply that comes after the deadline finds no waiter, so it moves nothing.
    #[test]
    fn a_reply_after_the_deadline_moves_nothing() {
        let mut app = super::A2aApp::new();
        let task_id = dispatch_opening(&mut app, "corr-late");
        app.expire_a2a_waiters(tokio::time::Instant::now() + std::time::Duration::from_hours(1));

        fulfill(
            &mut app,
            "corr-late",
            &bob_key(),
            &task_reply(&task_id, "TASK_STATE_COMPLETED"),
        );

        assert_eq!(
            app.tasks[&task_id].state,
            TaskState::Submitted,
            "only a reply to a parked call is reconciled"
        );
    }

    /// Only dispatch opens a record on this side: a reply that names a task we hold none of, such
    /// as the `GetTask` of a task that was reaped, creates none.
    #[test]
    fn a_reply_for_a_task_we_hold_no_record_of_creates_none() {
        let mut app = super::A2aApp::new();
        app.a2a_waiters.push(waiter("corr-get"));
        let task_id = TaskId::random();

        fulfill(
            &mut app,
            "corr-get",
            &bob_key(),
            &task_reply(&task_id, "TASK_STATE_WORKING"),
        );

        assert!(app.tasks.is_empty(), "a reply never opens a record");
    }

    /// A terminal reply from the worker repairs a terminal frame that we missed, and the same
    /// reply from another peer, which we also called, does not.
    #[test]
    fn a_terminal_reply_closes_the_record_only_from_its_worker() {
        let mut app = super::A2aApp::new();
        let task_id = dispatch_opening(&mut app, "corr-open");
        fulfill(
            &mut app,
            "corr-open",
            &bob_key(),
            &task_reply_at(&task_id, "TASK_STATE_WORKING", 50),
        );
        let mut to_carol = waiter("corr-carol");
        to_carol.peer_key = carol_key();
        app.a2a_waiters.push(to_carol);
        let done = task_reply(&task_id, "TASK_STATE_COMPLETED");

        fulfill(&mut app, "corr-carol", &carol_key(), &done);
        assert_eq!(
            app.tasks[&task_id].state,
            TaskState::Working,
            "carol holds no say over the task of bob"
        );

        app.a2a_waiters.push(waiter("corr-bob"));
        fulfill(&mut app, "corr-bob", &bob_key(), &done);
        assert_eq!(
            app.tasks[&task_id].state,
            TaskState::Completed,
            "the worker's terminal state is final truth"
        );
    }

    /// The check-in clock moves when a follow-up is sent, never when a reply is reconciled: a
    /// peer that wraps a `GetTask` reply as a `SendMessage` result must not hold off the alarm.
    #[test]
    fn no_reply_restarts_the_check_in_clock() {
        let mut app = super::A2aApp::new();
        let task_id = dispatch_opening(&mut app, "corr-open");
        let long_ago = Instant::now()
            .checked_sub(std::time::Duration::from_secs(600))
            .expect("the test instant is far from the clock origin");
        app.tasks
            .get_mut(&task_id)
            .expect("dispatch opened the record that this test sets back")
            .last_skill_activity = long_ago;

        fulfill(
            &mut app,
            "corr-open",
            &bob_key(),
            &task_reply(&task_id, "TASK_STATE_WORKING"),
        );

        assert_eq!(
            app.tasks[&task_id].last_skill_activity, long_ago,
            "a reply leaves the clock where it was"
        );
    }

    /// A worker that left cannot answer: the opening that waits on it is refused, and its record
    /// goes with it, before the tasks of the peer are ended. The caller never got that task id, so
    /// no `task_timeout` may name it.
    #[test]
    fn a_peer_that_left_takes_the_record_of_its_opening_without_a_task_timeout() {
        let mut app = super::A2aApp::new_with_io(super::SurfacedIo::new(Output::silent()));
        let task_id = dispatch_opening(&mut app, "corr-left");
        app.tasks.insert(
            TaskId::random(),
            TaskRecord::open(
                (bob_key(), Nickname::from("bob")),
                TaskRole::Initiator,
                TaskState::Working,
                Instant::now(),
            ),
        );

        app.peer_left((&bob_key(), &Nickname::from("bob")));

        assert!(
            !app.tasks.contains_key(&task_id),
            "the caller was told that the peer left, so no task of it remains"
        );
        app.drain_surfaced_ring();
        let timeouts = app
            .surfaced
            .poll_since(None)
            .events
            .iter()
            .filter(|item| matches!(item.event, OutputEvent::TaskTimeout { .. }))
            .count();
        assert_eq!(
            timeouts, 1,
            "the task that the caller knew is timed out for the peer that left, and the opening is not"
        );
    }
}
