use std::time::{Duration, Instant};

use tokio::time::Instant as TokioInstant;

use crate::a2a::app::{A2aApp, A2aResponder};
use crate::a2a::ipc::IpcCommand;
use crate::a2a::rpc::{A2aOp, A2aRequest};
use crate::a2a::session::SessionRequest;
use crate::a2a::wire;
use crate::output;
use agent_habilis_mesh::daemon::app::{IpcRequest, NodeDriver};
use agent_habilis_mesh::daemon::ctx::HandlerCtx;
use agent_habilis_mesh::daemon::state::EventLoopState;
use agent_habilis_mesh::daemon::state_file::StateFile;
use agent_habilis_mesh::gossip::app::{AppClass, InboundApp, NodeApp};
use agent_habilis_mesh::lookup::add_peer_addr;
use agent_habilis_mesh::protocol::message::MessageBody;
use agent_habilis_mesh::protocol::{AppTag, Channel, Message, MessageKind, Nickname};

#[async_trait::async_trait]
impl NodeApp for A2aApp {
    fn classify(&self, message: &Message) -> AppClass {
        classify(message)
    }

    async fn on_app_frame(
        &mut self,
        frame: InboundApp<'_>,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool {
        let InboundApp {
            message,
            surfaceable,
        } = frame;
        // A directed, correlated RPC frame (request/response) is plumbing —
        // never retained. Everything else is content routed to its lifecycle
        // handler, which decides loggability.
        let tag = message.kind.app_tag().map(AppTag::as_str);
        if tag == Some(wire::REQ) || tag == Some(wire::RESP) {
            handle_a2a_rpc(message, state, self, ctx).await;
            return false;
        }
        route_content(message, surfaceable, self, ctx)
    }

    fn surface_logical(&mut self, logical: &Message, surfaceable: bool, ctx: &HandlerCtx<'_>) {
        surface_logical(logical, surfaceable, self, ctx);
    }

    fn on_meta_applied(
        &mut self,
        author: &Nickname,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) {
        adopt_meta_endpoint(author, state, ctx);
    }

    async fn on_meshed(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        // Re-publish our card now that we're meshed: the home relay is homed by
        // this point, so the card's endpoint hint gets a real dial path (the
        // startup publish may have had none). A no-op if the address is
        // unchanged.
        publish_own_card(state, ctx).await;
    }

    async fn on_peer_left(
        &mut self,
        nickname: &Nickname,
        _state: &mut EventLoopState,
        _ctx: &HandlerCtx<'_>,
    ) {
        // Clone the render sink into a local so it doesn't alias the `&mut
        // self` passed alongside it. Tasks first, then waiters: a waiter must
        // never resolve while its task record is still live.
        let out = self.output.clone();
        crate::a2a::task::fail_tasks_for_departed_peer(&mut self.tasks, nickname, &out);
        self.fail_a2a_waiters_for_peer(nickname);
    }
}

#[async_trait::async_trait]
impl NodeDriver for A2aApp {
    type Session = SessionRequest;
    type Http = A2aRequest;
    type Ipc = IpcCommand;

    fn init_state_file(&self, state_file: Option<&StateFile>) {
        if let (Some(state_file), Some((port, token))) = (state_file, self.a2a_discovery()) {
            // The `a2a_port` / `a2a_token` keys are a documented contract — the
            // manual specifies them and the skills read them — so they are named
            // here, in the layer that owns A2A, and merged into a state file the
            // engine writes without interpreting.
            let mut fields = serde_json::Map::new();
            fields.insert("a2a_port".to_owned(), port.into());
            fields.insert("a2a_token".to_owned(), token.to_owned().into());
            state_file.set_discovery(fields);
        }
    }

    async fn on_startup(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        publish_own_card(state, ctx).await;
    }

    async fn on_tick(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        // Clone the render sink into a local so it doesn't alias the `&mut self`
        // passed alongside it.
        let out = self.output.clone();
        crate::a2a::task::tick_task_sweep(state, self, ctx, &out).await;
        crate::a2a::task::tick_task_keepalive(state, self, ctx).await;
        // Chase down any stalled big (unlogged) shard groups: ask their authors
        // to re-send the missing shards from their sender-side cache.
        crate::a2a::send::send_shard_repair_requests(ctx.mesh, ctx.author, state, ctx.sender).await;
    }

    async fn on_shutdown(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        // Retract our meta entry first so the merge frame is queued ahead of
        // `Left` and rides the same propagation window the engine sleeps after
        // this hook.
        retract_own_meta_entry(state, ctx).await;
        // Empty out any parked A2A-call waiters so a held call returns a
        // clean timeout, then close the blob-serving endpoint (dropping its
        // store removes the `<nick>.blobs/` spool).
        self.close_a2a_waiters();
        if let Some(server) = self.blob_server.take() {
            server.shutdown().await;
        }
    }

    fn earliest_deadline(&self) -> Option<TokioInstant> {
        self.earliest_a2a_deadline()
    }

    fn expire_deadlines(&mut self, now: TokioInstant) {
        self.expire_a2a_waiters(now);
    }

    fn drain_surfaced(&mut self) {
        self.drain_surfaced_ring();
    }

    fn earliest_poll_deadline(&self) -> Option<TokioInstant> {
        self.surfaced.earliest_poll_deadline()
    }

    fn poll_deadline_elapsed(&mut self) {
        // Fulfill any waiter a same-instant event made ready (so it wins over
        // the timeout), then expire the rest.
        self.surfaced.fulfill_ready_poll_waiters();
        self.surfaced.expire_poll_waiters(TokioInstant::now());
    }

    fn close_poll_waiters(&mut self) {
        self.surfaced.close_poll_waiters();
    }

    async fn handle_session(
        &mut self,
        req: SessionRequest,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool {
        let out = self.output.clone();
        crate::a2a::send::handle_session_request(
            crate::a2a::send::SessionRequestParams {
                req,
                mesh: ctx.mesh,
                author: ctx.author,
                app: self,
            },
            state,
            ctx.sender,
            &out,
        )
        .await
    }

    async fn handle_http(
        &mut self,
        req: A2aRequest,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) {
        let A2aRequest { op, resp } = req;
        // A directed `message/send` (task creation) needs the synchronous gossip
        // request/response round-trip — the peer mints the task id and returns the
        // Task. Route it through the waiter so the HTTP handler's oneshot resolves
        // when the peer answers (or times out); the event loop is never blocked.
        if let A2aOp::SendMessage {
            to: Some(peer),
            message,
        } = op
        {
            crate::a2a::send::broadcast_a2a_call(
                crate::a2a::send::BroadcastA2aCallParams {
                    mesh: ctx.mesh,
                    author: ctx.author,
                    peer,
                    method: "SendMessage",
                    params: serde_json::json!({ "message": message }),
                    timeout: Duration::from_secs(30),
                    responder: A2aResponder::Rpc(resp),
                },
                state,
                self,
                ctx.sender,
            )
            .await;
            return;
        }
        let out = self.output.clone();
        let port = self.a2a_port();
        let outcome = crate::a2a::rpc::handle_op(
            crate::a2a::rpc::HandleOpParams {
                op,
                mesh: ctx.mesh,
                author: ctx.author,
                our_pubkey: ctx.our_pubkey,
                a2a_port: port,
                app: self,
            },
            state,
            ctx.sender,
            &out,
        )
        .await;
        let _ = resp.send(outcome);
    }

    async fn handle_ipc(
        &mut self,
        req: IpcRequest<'_, IpcCommand>,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool {
        let out = self.output.clone();
        crate::a2a::ipc::handle_ipc_command(
            crate::a2a::ipc::IpcDispatchParams {
                cmd: req.cmd,
                resp_tx: req.resp,
                mesh: ctx.mesh,
                name: req.name,
                author: ctx.author,
                app: self,
            },
            state,
            ctx.sender,
            &out,
        )
        .await
    }
}

/// Publish this member's `AgentCard` at meta `/peers/<nick>/card` — one of the
/// daemon's two [`merge_own_meta_entry`] lifecycle merges (the card is the
/// peer's canonical A2A self-description, architectural rather than app
/// state). Unmeshed it buffers/backfills like any state event; agent-side
/// facts (model/harness/host) stay the agent's merge.
async fn publish_own_card(state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    let seal_b58 = bs58::encode(state.identity.seal_public()).into_string();
    let card = crate::a2a::card::own_card(ctx.author, ctx.our_pubkey, &seal_b58);
    // Fold our stable dial hint (EndpointId + home relay) into the card so a peer
    // that has synced the meta doc can dial us without a gossiped `PeerInfo`.
    // Re-publishing with an unchanged address is a no-op (an automerge change
    // with no ops), so this is safe to call repeatedly.
    let mut merge = crate::a2a::card::publish_merge(ctx.author, &card);
    merge["peers"][ctx.author.as_str()]["card"]["endpoint"] =
        crate::a2a::card::endpoint_hint(ctx.endpoint);
    merge_own_meta_entry(state, ctx, merge, false).await;
}

/// Delete this member's whole `/peers/<nick>` meta entry on graceful leave —
/// [`publish_own_card`]'s counterpart. Only self-retraction exists: the
/// card-forgery gate rejects the delete from any other author, so a peer that
/// dies without a goodbye leaves a stale entry that persists until a new
/// session under the same nickname (identity is per-session; nicknames are
/// never burned) overwrites it and eventually leaves cleanly. A foreign
/// non-card write racing the delete can survive the merge (add wins), but
/// peers only ever write their own subtree.
async fn retract_own_meta_entry(state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    let merge = crate::a2a::card::retract_merge(ctx.author);
    merge_own_meta_entry(state, ctx, merge, true).await;
}

/// The daemon's write path for its own `/peers/<nick>` meta entry. The daemon
/// is an ordinary channel writer bound by the same rule as its agent — author
/// only your own subtree, as an RFC 7386 merge through the one shared
/// pipeline (`broadcast_state_merge`). It makes exactly two lifecycle merges:
/// the card publish on join/re-mesh and the whole-entry retraction on
/// graceful leave. Best-effort — a failure is logged, never blocks the
/// lifecycle edge that triggered it.
///
/// `farewell` mirrors the signed frame over the unicast plane — the
/// retraction only: a leave-time frame dropped into a silently-linkless
/// overlay dies with the leaver, and anti-entropy can never heal it (see
/// `gossip::unicast_farewell`).
async fn merge_own_meta_entry(
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
    merge: serde_json::Value,
    farewell: bool,
) {
    match agent_habilis_mesh::gossip::broadcast_state_merge(
        state,
        agent_habilis_mesh::gossip::StateMergeParams {
            mesh: ctx.mesh,
            author: ctx.author,
            merge,
            sender: ctx.sender,
            sink: ctx.sink,
            channel: Channel::Meta,
            // Daemon plumbing: the agent didn't write this, so don't surface it
            // as a "you changed shared state" event (nor race a fetch long-poll).
            surface: false,
        },
    )
    .await
    {
        Ok(Some(bytes)) if farewell => {
            agent_habilis_mesh::gossip::unicast_farewell(state, &bytes);
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "own /peers entry merge failed");
        }
    }
}

/// Whether a surfaced event belongs in the `poll`/`fetch` history — an explicit
/// allow-list of the documented pollable contract (chat, presence joined/left,
/// content task legs, and the transient `ping_report`/`peer_timeout`/
/// `peer_return`/`task_timeout`/`fork`). Deliberately an allow-list, not
/// "everything except X": operational notices (`info`/`error`/`msg_posted`) and
/// startup events (`ready`/`mesh_id`) also flow through the same `Output` tap,
/// and must NOT enter the ring. The `task` `Progress` beat is excluded too (a
/// liveness widget update, never a retained record).
/// Whether a surfaced event should wake a parked `poll --long` bell: anything
/// the agent must see (`is_visible`) or act on (a task interaction). State and
/// meta document echoes, `fork`, and presence `alive` beats stay in the ring —
/// consumable in the next batch — but never ring the bell on their own, so a
/// self meta report at session start cannot cost the agent a wake-up turn.
pub(crate) fn wakes(event: &output::OutputEvent) -> bool {
    use output::OutputEvent;
    output::is_visible(event)
        || matches!(
            event,
            OutputEvent::Task { .. }
                | OutputEvent::TaskMessage { .. }
                | OutputEvent::TaskTimeout { .. }
        )
}

pub(crate) fn is_pollable(event: &output::OutputEvent) -> bool {
    use output::OutputEvent;
    match event {
        OutputEvent::Message { .. }
        | OutputEvent::Presence { .. }
        | OutputEvent::PingReport { .. }
        | OutputEvent::PeerTimeout { .. }
        | OutputEvent::PeerReturn { .. }
        | OutputEvent::TaskTimeout { .. }
        | OutputEvent::TaskMessage { .. }
        | OutputEvent::StateChanged { .. }
        | OutputEvent::Fork { .. } => true,
        // A task liveness beat (a `Progress` widget update) is never retained;
        // the per-frame classification is the source of truth for it.
        OutputEvent::Task { msg, .. } => !classify(msg).beat,
        OutputEvent::Info { .. }
        | OutputEvent::Error { .. }
        | OutputEvent::MsgPosted { .. }
        | OutputEvent::Ready { .. } => false,
    }
}

/// Per-tag/payload classification of an inbound a2a `App` frame — see
/// [`AppClass`]. Encodes in one place the wire policy the engine used to derive
/// from hardcoded tag checks.
fn classify(message: &Message) -> AppClass {
    let tag = message.kind.app_tag().map(AppTag::as_str);
    // App payloads log (chat/status/artifact) only. RPC request/response frames
    // are plumbing: the result reaches the requester via its parked waiter,
    // never the chat log / poll-fetch buffer. A tag outside the a2a taxonomy is
    // not loggable (see `valid` — it never surfaces at all).
    let loggable = matches!(tag, Some(wire::MSG | wire::STATUS | wire::ARTIFACT));
    // A task liveness beat: a status update marked `mesh:beat`.
    let beat = message.kind.is_app(wire::STATUS)
        && crate::a2a::gossip::status_payload(message)
            .is_ok_and(|payload| crate::a2a::gossip::is_beat(&payload));
    // The A2A boundary gate for a logical frame: a chat/task Message, status, or
    // artifact must parse and agree with its frame *and* carry the addressing its
    // tag mandates. `a2a_msg` is broadcast-only (`to: None`); status/artifact and
    // RPC req/resp are directed-only (`to: Some`). Enforcing the addressing here
    // is what stops a forged broadcast status/artifact (`to: None`) — which would
    // otherwise pass, then retain + inbound-push on every peer — and a directed
    // `a2a_msg` from crossing the gate. A tag outside the a2a taxonomy is invalid:
    // a signed frame with an arbitrary tag must not cross to api consumers, so
    // drop it whole. `None` is a non-App frame the engine gates itself.
    let directed = matches!(message.kind, MessageKind::App { to: Some(_), .. });
    let valid = match tag {
        Some(wire::MSG) => !directed && crate::a2a::gossip::chat_payload(message).is_ok(),
        Some(wire::STATUS) => directed && crate::a2a::gossip::status_payload(message).is_ok(),
        Some(wire::ARTIFACT) => directed && crate::a2a::gossip::artifact_payload(message).is_ok(),
        Some(wire::REQ | wire::RESP) => directed,
        None => true,
        Some(_) => false,
    };
    // Only a broadcast chat `a2a_msg` carries `seq`/parents and so participates
    // in fork-detection + the cross-author DAG.
    let chained = message.kind.is_app(wire::MSG);
    // a2a's directed tags — worker status/artifact push and RPC request/response —
    // arrive sealed (encrypted) to their addressee, so a directed frame addressed
    // to us must unseal-or-drop. `a2a_msg` is broadcast chat (never
    // directed/sealed), and an unknown tag is never sealed.
    let sealed = matches!(
        tag,
        Some(wire::STATUS | wire::ARTIFACT | wire::REQ | wire::RESP)
    );
    AppClass {
        loggable,
        beat,
        valid,
        chained,
        sealed,
    }
}

/// Route a content frame — chat, or one of the three task shapes — to its
/// lifecycle handler. Returns whether the frame is loggable (a directed
/// frame addressed elsewhere, or a liveness beat, is not).
fn route_content(
    message: &Message,
    surfaceable: bool,
    app: &mut A2aApp,
    ctx: &HandlerCtx<'_>,
) -> bool {
    match &message.kind {
        // `a2a_msg` is broadcast-only chat: a directed (`to: Some`) MSG is
        // malformed and must never be surfaced/logged by a relay, so require
        // `to: None` here — a directed MSG falls through to the no-op arm.
        MessageKind::App { tag, to: None, .. } if tag.as_str() == wire::MSG => {
            handle_msg(&app.output, message, surfaceable)
        }
        MessageKind::App {
            tag, to: Some(to), ..
        } if tag.as_str() == wire::STATUS || tag.as_str() == wire::ARTIFACT => handle_task_leg(
            TaskLegParams {
                message,
                to,
                surfaceable,
            },
            app,
            ctx,
        ),
        MessageKind::App { .. }
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::State
        | MessageKind::Meta
        | MessageKind::LinkState => false,
    }
}

/// The value cluster [`handle_task_leg`] needs beyond its `app`/`ctx` handles
/// — shared by both call sites ([`route_content`] and [`surface_logical`]).
#[derive(Clone, Copy)]
struct TaskLegParams<'a> {
    message: &'a Message,
    to: &'a Nickname,
    surfaceable: bool,
}

/// Process an inbound task leg (a task-related `a2a_status` or `a2a_artifact`)
/// for its addressee: advance the coarse state machine (addressee only — a
/// third-party relay tracks nothing), then surface it via the lifecycle layer.
fn handle_task_leg(leg: TaskLegParams<'_>, app: &mut A2aApp, ctx: &HandlerCtx<'_>) -> bool {
    let TaskLegParams {
        message,
        to,
        surfaceable,
    } = leg;
    // Only the addressee is a party: a third-party relay tracks nothing. The
    // receiver's body is already unsealed here, so the task id parses out of it.
    if to == ctx.author
        && let Some(task_id) = crate::a2a::gossip::frame_task_id(message)
    {
        crate::a2a::task::ingest(
            &mut app.tasks,
            crate::a2a::task::IngestLegParams {
                frame: message,
                task_id: &task_id,
                mine: false,
                now: Instant::now(),
            },
        );
    }
    handle_task(
        &app.output,
        TaskDisplayParams {
            message,
            to,
            surfaceable,
            self_author: ctx.author,
        },
    )
}

/// Returns whether the message should be logged (pushed to the poll buffer).
/// `a2a_msg` is mesh broadcast chat — always loggable. `surfaceable` gates
/// only the *display*: a pre-join message is still logged/relayed but not
/// printed.
fn handle_msg(out: &output::Output, message: &Message, surfaceable: bool) -> bool {
    if message.kind.is_app(wire::MSG) {
        if surfaceable {
            out.print_message(message);
        }
        true
    } else {
        false
    }
}

/// The value cluster [`handle_task`] needs beyond its `out` sink handle.
#[derive(Clone, Copy)]
struct TaskDisplayParams<'a> {
    message: &'a Message,
    to: &'a Nickname,
    surfaceable: bool,
    self_author: &'a Nickname,
}

/// A task leg: surfaced + logged only by the addressee (`to == self_author`)
/// and, via the sender's echo path, the sender itself — third parties relay it
/// without retaining. Returns whether to **log** (content legs only — a beat
/// status is liveness plumbing, surfaced as a `task_progress` widget event but
/// never retained). `surfaceable` gates only the *display* (join-horizon),
/// never the relay/log.
fn handle_task(out: &output::Output, params: TaskDisplayParams<'_>) -> bool {
    let TaskDisplayParams {
        message,
        to,
        surfaceable,
        self_author,
    } = params;
    if to != self_author {
        return false;
    }
    if surfaceable {
        out.print_task(message, false);
    }
    // Content legs log; a liveness beat (a status marked `mesh:beat`) is
    // plumbing — surfaced as a `task_progress` widget event but never retained.
    !crate::a2a::gossip::status_payload(message)
        .is_ok_and(|payload| crate::a2a::gossip::is_beat(&payload))
}

/// Surface a reassembled multipart body (a `Msg` or a `Task` content leg)
/// through the same lifecycle path an unsplit message of that kind takes. The
/// raw shards were already retained; this only surfaces the logical view, so it
/// does **not** re-retain or re-index.
fn surface_logical(logical: &Message, surfaceable: bool, app: &mut A2aApp, ctx: &HandlerCtx<'_>) {
    match &logical.kind {
        // `a2a_msg` is broadcast-only chat (task legs never shard through here —
        // `message/send` rides RPC, status/artifact are small).
        MessageKind::App { tag, to: None, .. } if tag.as_str() == wire::MSG => {
            handle_msg(&app.output, logical, surfaceable);
        }
        MessageKind::App {
            tag, to: Some(to), ..
        } if tag.as_str() == wire::STATUS || tag.as_str() == wire::ARTIFACT => {
            handle_task_leg(
                TaskLegParams {
                    message: logical,
                    to,
                    surfaceable,
                },
                app,
                ctx,
            );
        }
        // Only content frames are ever split (the sender refuses to split
        // anything else), so other kinds never reach here.
        MessageKind::App { .. }
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::State
        | MessageKind::Meta
        | MessageKind::LinkState => {}
    }
}

/// Route an A2A RPC frame: a request addressed to us is served (and a response
/// broadcast back); a response to one of our calls resolves its waiter (matched
/// by `corr` *and* the responding peer). Frames addressed to another peer are
/// relayed only. Plumbing — never logged/retained.
async fn handle_a2a_rpc(
    message: &Message,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    ctx: &HandlerCtx<'_>,
) {
    let (tag, corr) = match &message.kind {
        MessageKind::App {
            tag,
            to: Some(to),
            corr: Some(corr),
        } if to == ctx.author => (tag.as_str(), corr),
        MessageKind::App { .. }
        | MessageKind::Presence { .. }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::Ping
        | MessageKind::Pong { .. }
        | MessageKind::State
        | MessageKind::StateDigest
        | MessageKind::Meta
        | MessageKind::MetaDigest
        | MessageKind::LinkState => return,
    };
    // The engine keeps `corr` generic (a length-bounded string); the a2a layer
    // restores the UUID contract. A non-UUID corr never came from a correct
    // client — drop the frame rather than key the waiter registry on it.
    if crate::a2a::A2aRpcId::from_uuid_str(corr.as_str()).is_none() {
        return;
    }
    if tag == wire::REQ {
        handle_a2a_req(
            A2aReqParams {
                message,
                corr: &corr.clone(),
            },
            state,
            app,
            ctx,
        )
        .await;
    } else if tag == wire::RESP && app.has_a2a_waiter(corr, &message.author) {
        // Only act on a response to a call WE actually issued (a matching
        // outstanding waiter). An unsolicited response is ignored — otherwise
        // any member could inject phantom initiator-side task records by forging
        // responses. Adopt the authoritative `Task` a `SendMessage` returned.
        app.adopt_returned_task(&message.author, message.body.as_str());
        app.fulfill_a2a_waiter(corr, &message.author, message.body.as_str());
    }
}

/// One inbound `a2a_req` frame's request body plus the correlation id its
/// `a2a_resp` must echo back — grouped so [`handle_a2a_req`] stays within the
/// argument budget alongside its `state`/`app`/`ctx` handles.
struct A2aReqParams<'a> {
    message: &'a Message,
    corr: &'a agent_habilis_mesh::protocol::CorrId,
}

/// Serve one inbound `a2a_req` addressed to us: classify the JSON-RPC request
/// against the safe method set (`gossip_rpc::classify`), run the resulting
/// action, and broadcast an `a2a_resp` back to the requester echoing `corr`.
async fn handle_a2a_req(
    req: A2aReqParams<'_>,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    ctx: &HandlerCtx<'_>,
) {
    use crate::a2a::gossip_rpc::Served;
    let A2aReqParams { message, corr } = req;
    let requester = message.author.clone();
    // Parse the JSON-RPC envelope; a malformed request is a parse-error reply.
    let outcome: Result<serde_json::Value, crate::a2a::rpc::RpcError> = match serde_json::from_str::<
        serde_json::Value,
    >(
        message.body.as_str(),
    ) {
        Ok(envelope) => {
            let method = envelope["method"].as_str().unwrap_or_default();
            match crate::a2a::gossip_rpc::classify(method, &envelope["params"], &requester, app) {
                Served::Op(op) => {
                    let out = app.output.clone();
                    crate::a2a::rpc::handle_op(
                        crate::a2a::rpc::HandleOpParams {
                            op,
                            mesh: ctx.mesh,
                            author: ctx.author,
                            our_pubkey: ctx.our_pubkey,
                            // No local HTTP port on the gossip path; only card
                            // ops read it, and those aren't in the safe set here.
                            a2a_port: 0,
                            app,
                        },
                        state,
                        ctx.sender,
                        &out,
                    )
                    .await
                }
                Served::Ingest(payload) => ingest_remote_message(&payload, &requester, app, ctx),
                Served::ShardRepair { group, missing } => {
                    resend_cached_shards(
                        ShardRepairParams {
                            group: &group,
                            missing: &missing,
                            requester: &requester,
                        },
                        state,
                        ctx,
                    )
                    .await
                }
                Served::Reject(error) => Err(error),
            }
        }
        Err(error) => Err(crate::a2a::rpc::RpcError::invalid_params(format!(
            "malformed JSON-RPC request: {error}"
        ))),
    };
    let response = match outcome {
        Ok(result) => serde_json::json!({ "result": result }),
        Err(error) => {
            serde_json::json!({ "error": { "code": error.code, "message": error.message } })
        }
    };
    let Ok(body) = MessageBody::new(response.to_string()) else {
        tracing::warn!("a2a rpc response body invalid");
        return;
    };
    // Seal the response to the requester — a relay forwards it but cannot read it.
    let body = match crate::a2a::send::seal_directed(state, &requester, &body) {
        Ok(sealed) => sealed,
        Err(error) => {
            tracing::warn!(%error, "cannot seal a2a rpc response — dropping");
            return;
        }
    };
    let kind = MessageKind::App {
        tag: AppTag::from(wire::RESP),
        to: Some(requester),
        corr: Some(corr.clone()),
    };
    // Directed reply over the shared RPC sender: unicast, transparently
    // splitting a large sealed response into shard frames.
    if let Err(error) = crate::a2a::send::send_directed_rpc(
        crate::a2a::send::DirectedRpcParams {
            mesh: ctx.mesh,
            author: ctx.author,
            kind,
            body,
        },
        state,
        ctx.sender,
    )
    .await
    {
        tracing::warn!(%error, "a2a rpc response send failed");
    }
}

/// The value cluster [`resend_cached_shards`] needs beyond its `state`/`ctx`
/// handles.
struct ShardRepairParams<'a> {
    group: &'a agent_habilis_mesh::protocol::ShardGroup,
    missing: &'a [u32],
    requester: &'a Nickname,
}

/// Serve a `shard/repair` ask: re-deliver the named cached frames of one of our
/// big (unlogged) outbound groups to the requester. The frames are the raw,
/// already-signed shards we cached at send time (`state.shard_cache`), so they
/// reassemble on the requester exactly as the original send would have.
async fn resend_cached_shards(
    params: ShardRepairParams<'_>,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) -> Result<serde_json::Value, crate::a2a::rpc::RpcError> {
    let ShardRepairParams {
        group,
        missing,
        requester,
    } = params;
    let frames = state.shard_cache.frames(group, missing);
    let mut resent = 0usize;
    for bytes in frames {
        let Ok(msg) = Message::parse(&bytes) else {
            continue; // never: the cache holds frames we serialized ourselves
        };
        if let Some(to) = agent_habilis_mesh::protocol::message::sole_addressee(&msg.kind)
            && to != requester
        {
            continue;
        }
        if agent_habilis_mesh::transport::deliver(&msg, bytes, state, ctx.sender)
            .await
            .is_ok()
        {
            resent += 1;
        }
    }
    tracing::debug!(
        target: "agent_habilis_mesh::gossip",
        %group,
        requested = missing.len(),
        resent,
        "served a shard repair request"
    );
    Ok(serde_json::json!({ "resent": resent }))
}

/// A `message/send` directed at us over gossip-RPC — we are the task's A2A
/// **server** (the worker). A message with no `taskId` opens a new task (we
/// mint the id); one with a `taskId` is a follow-up. Either way we advance our
/// coarse machine, surface the message to our skill, and return the
/// authoritative `Task`.
fn ingest_remote_message(
    payload: &crate::a2a::Message,
    requester: &Nickname,
    app: &mut A2aApp,
    ctx: &HandlerCtx<'_>,
) -> Result<serde_json::Value, crate::a2a::rpc::RpcError> {
    use crate::a2a::task::{LegInfo, LegKind};
    let now = Instant::now();
    let (task_id, kind) = match &payload.task_id {
        Some(task_id) => (task_id.clone(), LegKind::Text),
        None => (crate::a2a::TaskId::random(), LegKind::Offer),
    };
    // A follow-up (taskId set) may only be driven by the task's own party. The
    // requester is signature-verified, but signature ≠ party: without this check
    // any mesh member could drive and read a task they are not part of.
    if kind == LegKind::Text {
        match app.tasks.get(&task_id) {
            None => {
                return Err(crate::a2a::rpc::RpcError::invalid_params(
                    "SendMessage names a taskId with no task on this peer",
                ));
            }
            Some(rec) if &rec.peer != requester => {
                return Err(crate::a2a::rpc::RpcError {
                    code: -32003,
                    message: "not a party to the task named by taskId".to_string(),
                });
            }
            Some(_) => {}
        }
    }
    crate::a2a::task::apply(
        &mut app.tasks,
        &LegInfo {
            task_id: &task_id,
            peer: requester,
            kind,
            mine: false,
            fraction: None,
        },
        now,
    );
    // Surface the incoming message to our skill (a `task` event, kind:message).
    let rec_state = app.tasks.get(&task_id).map(|rec| rec.state);
    let text = crate::a2a::gossip::display_text(payload);
    app.output.task_message(&output::TaskMessageLeg {
        id: payload.message_id.as_str(),
        mesh: ctx.mesh.as_str(),
        author: requester.as_str(),
        peer: ctx.author.as_str(),
        task_id: task_id.as_str(),
        state: rec_state,
        text: &text,
        is_self: false,
    });
    // A2A v1.0 `SendMessage` returns a `SendMessageResponse` oneof — creation
    // yields `{"task": <Task>}`.
    app.tasks
        .get(&task_id)
        .map(|rec| serde_json::json!({ "task": crate::a2a::rpc::task_object(&task_id, rec, ctx.mesh) }))
        .ok_or_else(|| crate::a2a::rpc::RpcError::internal("task record missing after ingest"))
}

/// Adopt `author`'s published endpoint hint (`/peers/<nick>/card/endpoint`) into
/// the dial book — the meta-doc counterpart to `handle_peer_info`. Binds
/// `peer_endpoints[author] = eid` and seeds iroh's address book via
/// [`add_peer_addr`], so a directed unicast can reach a peer we learned about
/// from the synced meta doc even before its gossiped `PeerInfo` arrives. Both
/// are last-writer-wins / additive, so this never conflicts with `PeerInfo`. It
/// does **not** graft a gossip link — mesh formation stays `PeerInfo`'s job.
fn adopt_meta_endpoint(author: &Nickname, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    let doc = state.meta_doc.to_json();
    let Some((peer_id, peer_addr)) = crate::a2a::card::peer_endpoint(&doc, author) else {
        return;
    };
    // Never bind our own endpoint or the rendezvous pseudo-node.
    if peer_id == ctx.endpoint.id() || peer_id == ctx.rendezvous_id {
        return;
    }
    state.peer_endpoints.insert(author.clone(), peer_id);
    if let Err(error) = add_peer_addr(ctx.endpoint, peer_addr) {
        tracing::debug!(%error, %author, "could not seed a meta-doc endpoint into the address book");
    }
}

#[cfg(test)]
mod classify_tests {
    use super::classify;
    use crate::a2a::wire;
    use agent_habilis_mesh::protocol::{
        AppFrameParams, AppTag, MeshId, Message, MessageBody, MessageId, MessageKind, Nickname,
    };

    fn mesh() -> MeshId {
        MeshId::from("💬test")
    }

    fn app_frame(tag: &str) -> Message {
        Message::new_app(
            &mesh(),
            &Nickname::from("author"),
            AppFrameParams {
                tag: AppTag::from(tag),
                to: None,
                corr: None,
                body: MessageBody::from("{}"),
            },
        )
    }

    // A well-formed broadcast chat frame: its frame id equals the payload's
    // messageId, so `chat_payload` accepts it — the `to` is the only variable.
    fn chat_frame(to: Option<Nickname>) -> Message {
        let sw = mesh();
        let payload = crate::a2a::gossip::chat_message(&sw, "hi");
        let id = MessageId::new(payload.message_id.as_str()).unwrap();
        let body = crate::a2a::gossip::payload_body(&payload).unwrap();
        Message::new_app(
            &sw,
            &Nickname::from("author"),
            AppFrameParams {
                tag: AppTag::from(wire::MSG),
                to,
                corr: None,
                body,
            },
        )
        .with_id(id)
    }

    // A well-formed task-status frame (its payload's contextId names the mesh).
    fn status_frame(to: Option<Nickname>) -> Message {
        let sw = mesh();
        let task_id = crate::a2a::TaskId::random();
        let status = crate::a2a::gossip::status_update(
            &sw,
            crate::a2a::gossip::StatusUpdateParams {
                task_id: &task_id,
                state: crate::a2a::TaskState::Working,
                note: None,
                metadata: None,
            },
        );
        let body = crate::a2a::gossip::payload_body(&status).unwrap();
        Message::new_app(
            &sw,
            &Nickname::from("author"),
            AppFrameParams {
                tag: AppTag::from(wire::STATUS),
                to,
                corr: None,
                body,
            },
        )
    }

    #[test]
    fn msg_is_loggable_but_rpc_is_not() {
        let msg = classify(&app_frame(wire::MSG));
        assert!(msg.loggable, "chat is loggable");
        assert!(msg.chained, "chat carries the per-author chain");
        let req = classify(&app_frame(wire::REQ));
        assert!(!req.loggable, "an RPC request is plumbing, not logged");
        assert!(!req.chained, "only chat is chained");
    }

    #[test]
    fn unknown_tag_is_invalid_and_not_loggable() {
        let cls = classify(&app_frame("admin"));
        assert!(!cls.valid, "an unknown tag fails the boundary gate");
        assert!(!cls.loggable, "an unknown tag is not logged");
        assert!(!cls.beat, "an unknown tag is not a beat");
        assert!(!cls.chained, "an unknown tag is not chained");
    }

    #[test]
    fn non_app_kinds_have_no_classification_effect() {
        let peer_info = classify(&Message::new_peer_info(
            &mesh(),
            &Nickname::from("author"),
            MessageBody::from("{}"),
        ));
        assert!(!peer_info.loggable);
        let _ = MessageKind::PeerInfo;
    }

    // ① A forged broadcast status (`to: None`) — even with a well-formed body —
    // must fail the boundary gate, so it is dropped before it can retain/push
    // on every peer. (`a2a_artifact` shares the directed-only rule.)
    #[test]
    fn broadcast_status_is_invalid() {
        let cls = classify(&status_frame(None));
        assert!(
            !cls.valid,
            "a2a_status is directed-only; a `to: None` status is a forged broadcast"
        );
    }

    // ② A directed `a2a_msg` (`to: Some`) is malformed — chat is broadcast-only.
    #[test]
    fn directed_msg_is_invalid() {
        let cls = classify(&chat_frame(Some(Nickname::from("bob"))));
        assert!(
            !cls.valid,
            "a2a_msg is broadcast-only; a directed one must not cross the gate"
        );
    }

    // No regression: legitimate addressing still classifies valid.
    #[test]
    fn legitimate_addressing_stays_valid() {
        assert!(
            classify(&status_frame(Some(Nickname::from("bob")))).valid,
            "a directed status is valid"
        );
        assert!(
            classify(&chat_frame(None)).valid,
            "a broadcast chat message is valid"
        );
    }
}
