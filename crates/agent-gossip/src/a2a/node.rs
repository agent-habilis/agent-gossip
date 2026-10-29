use std::time::{Duration, Instant};

use habilis_network::embed::{
    AppClass, EventLoopState, HandlerCtx, InboundApp, IpcRequest, NodeApp, NodeDriver,
};
use habilis_network::net::add_peer_addr;
use habilis_network::ops::{MergeCtx, StateMergeParams, broadcast_state_merge};
use habilis_network::protocol::message::Authored;
use habilis_network::protocol::{
    AppTag, Channel, DedupKey, Message, MessageBody, MessageKind, Nickname,
};
use habilis_network::runtime::state_file::StateFile;
use habilis_network::util::bounded_fifo_set::BoundedFifoSet;
use tokio::time::Instant as TokioInstant;

use crate::a2a::a2a_app::{A2aApp, A2aResponder};
use crate::a2a::ipc_command::IpcCommand;
use crate::a2a::receipt::{Receipt, ReceiptState, send_receipt};
use crate::a2a::rpc_error::{A2aOp, A2aRequest, RpcError};
use crate::a2a::session_request::SessionRequest;
use crate::a2a::{
    broadcast_params, card, ipc_command, rpc_error, served, status_update_params, task_record,
    tuning, wire,
};
use crate::cli::bell;
use crate::output;

/// The longest wait, in ticks, between two tries to publish a card that did not publish.
const CARD_RETRY_MAX_TICKS: u32 = 64;

#[async_trait::async_trait]
impl NodeApp for A2aApp {
    fn classify(&self, message: &Authored) -> AppClass {
        classify(message)
    }

    async fn on_app_frame(
        &mut self,
        frame: InboundApp<'_>,
        state: &mut EventLoopState,
        context: &HandlerCtx<'_>,
    ) -> bool {
        let InboundApp {
            message,
            surfaceable,
            ..
        } = frame;
        // A directed, correlated RPC frame (request/response) is plumbing —
        // never retained. Everything else is content routed to its lifecycle
        // handler, which decides loggability.
        let tag = message.kind.app_tag().map(AppTag::as_str);
        if tag == Some(wire::REQ) || tag == Some(wire::RESP) {
            handle_a2a_rpc(message, state, self, context).await;
            return false;
        }
        let routed = route_content(message, surfaceable, self, context);
        if routed.fresh_for_us {
            let receipt = Receipt {
                ref_id: message.id.as_str().to_owned(),
                state: ReceiptState::Delivered,
            };
            // Best effort: `send_receipt` logs its own failure, and a lost
            // receipt only leaves the sender without a delivery proof.
            let _ = send_receipt(state, context, &message.author, &receipt).await;
        }
        routed.loggable
    }

    fn on_meta_applied(
        &mut self,
        author: &Nickname,
        state: &mut EventLoopState,
        context: &HandlerCtx<'_>,
    ) {
        adopt_meta_endpoint(author, state, context);
    }

    async fn on_meshed(&mut self, state: &mut EventLoopState, context: &HandlerCtx<'_>) {
        // Re-publish our card now that we're meshed: the home relay is homed by
        // this point, so the card's endpoint hint gets a real dial path (the
        // startup publish may have had none). A no-op if the address is
        // unchanged.
        self.sync_own_card(state, context).await;
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
        task_record::fail_tasks_for_departed_peer(&mut self.tasks, nickname, &out);
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

    async fn on_startup(&mut self, state: &mut EventLoopState, context: &HandlerCtx<'_>) {
        self.sync_own_card(state, context).await;
    }

    async fn on_tick(&mut self, state: &mut EventLoopState, context: &HandlerCtx<'_>) {
        // Clone the render sink into a local so it doesn't alias the `&mut self`
        // passed alongside it.
        let out = self.output.clone();
        broadcast_params::retain_sent_legs(self, state);
        // A card that did not publish is tried again, with a longer wait after each failure:
        // no peer can send to us without it.
        if self.card_error.is_some() {
            if self.card_retry_in == 0 {
                self.sync_own_card(state, context).await;
            } else {
                self.card_retry_in -= 1;
            }
        }
        task_record::tick_task_sweep(state, self, context, &out).await;
        task_record::tick_task_keepalive(state, self, context).await;
        // Chase down any stalled big (unlogged) shard groups: ask their authors
        // to re-send the missing shards from their sender-side cache.
        broadcast_params::send_shard_repair_requests(
            context.mesh,
            context.author,
            state,
            context.sender,
        )
        .await;
    }

    async fn on_shutdown(&mut self, state: &mut EventLoopState, context: &HandlerCtx<'_>) {
        // Retract our meta entry first so the merge frame is queued ahead of
        // `Left` and rides the same propagation window the engine sleeps after
        // this hook.
        retract_own_meta_entry(state, context).await;
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
        self.drain_delivered_legs();
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
        request: SessionRequest,
        state: &mut EventLoopState,
        context: &HandlerCtx<'_>,
    ) -> bool {
        let out = self.output.clone();
        broadcast_params::handle_session_request(
            broadcast_params::SessionRequestParams {
                request,
                mesh: context.mesh,
                author: context.author,
                app: self,
            },
            state,
            context,
            &out,
        )
        .await
    }

    async fn handle_http(
        &mut self,
        request: A2aRequest,
        state: &mut EventLoopState,
        context: &HandlerCtx<'_>,
    ) {
        let A2aRequest { op, response } = request;
        // A directed `message/send` (task creation) needs the synchronous gossip
        // request/response round-trip — the peer mints the task id and returns the
        // Task. Route it through the waiter so the HTTP handler's oneshot resolves
        // when the peer answers (or times out); the event loop is never blocked.
        if let A2aOp::SendMessage {
            to: Some(peer),
            message,
        } = op
        {
            broadcast_params::broadcast_a2a_call(
                broadcast_params::BroadcastA2aCallParams {
                    mesh: context.mesh,
                    author: context.author,
                    peer,
                    method: "SendMessage",
                    params: serde_json::json!({ "message": message }),
                    timeout: Duration::from_secs(tuning::CALL_TIMEOUT_SECS),
                    responder: A2aResponder::Rpc(response),
                    file: None,
                },
                state,
                self,
                context.sender,
            )
            .await;
            return;
        }
        let out = self.output.clone();
        let port = self.a2a_port();
        let outcome = rpc_error::handle_op(
            rpc_error::HandleOpParams {
                op,
                mesh: context.mesh,
                author: context.author,
                our_pubkey: context.our_pubkey,
                a2a_port: port,
                app: self,
            },
            state,
            context.sender,
            &out,
        )
        .await;
        let _ = response.send(outcome);
    }

    async fn handle_ipc(
        &mut self,
        request: IpcRequest<'_, IpcCommand>,
        state: &mut EventLoopState,
        context: &HandlerCtx<'_>,
    ) -> bool {
        let out = self.output.clone();
        ipc_command::handle_ipc_command(
            ipc_command::IpcDispatchParams {
                command: request.command,
                resp_tx: request.response,
                mesh: context.mesh,
                name: request.name,
                author: context.author,
                endpoint: context.endpoint,
                app: self,
            },
            state,
            context,
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
///
/// The card goes out in several merges, each within one frame: a frame of a passworded gossip
/// is sealed, which makes it larger, and one merge of the whole card is then refused.
///
/// # Errors
/// The card does not fit the frames, or a merge was refused.
async fn publish_own_card(
    state: &mut EventLoopState,
    context: &HandlerCtx<'_>,
) -> anyhow::Result<()> {
    let seal_b58 = bs58::encode(state.identity().seal_public().as_bytes()).into_string();
    let card = card::own_card(context.author, context.our_pubkey, &seal_b58);
    // Fold our stable dial hint (EndpointId + home relay) into the card so a peer
    // that has synced the meta doc can dial us without a gossiped `PeerInfo`.
    // Re-publishing with an unchanged address is a no-op (an automerge change
    // with no ops), so this is safe to call repeatedly.
    let mut card = serde_json::to_value(&card)?;
    card["endpoint"] = card::endpoint_hint(context.endpoint);
    // The cut follows the size of the frame that the gossip really sends: the merge becomes an
    // automerge change, and a passworded gossip seals it. The same steps as the merge path.
    let merges = {
        let doc = state.doc(Channel::Meta);
        let identity = state.identity();
        let actor_seed = *identity.public().as_bytes();
        card::publish_merges(context.author, &card, |merge| {
            let Some(change) = doc.build_change(merge, &actor_seed)? else {
                return Ok(0);
            };
            let (wire, _plain) = doc.compose_wire_body(&change, None)?;
            Ok(
                Message::new_channel_event(context.mesh, context.author, wire, Channel::Meta)
                    .sign(identity)
                    .wire_len(),
            )
        })?
    };
    for merge in merges {
        merge_own_meta_entry(state, context, merge, false).await?;
    }
    Ok(())
}

/// Delete this member's whole `/peers/<nick>` meta entry on graceful leave —
/// [`publish_own_card`]'s counterpart. Only self-retraction exists: the
/// card-forgery gate rejects the delete from any other author, so a peer that
/// dies without a goodbye leaves a stale entry that persists until a new
/// session under the same nickname (identity is per-session; nicknames are
/// never burned) overwrites it and eventually leaves cleanly. A foreign
/// non-card write racing the delete can survive the merge (add wins), but
/// peers only ever write their own subtree.
async fn retract_own_meta_entry(state: &mut EventLoopState, context: &HandlerCtx<'_>) {
    let merge = card::retract_merge(context.author);
    // Best effort at leave: the failure is logged by the merge.
    let _ = merge_own_meta_entry(state, context, merge, true).await;
}

impl A2aApp {
    /// Publish our card, and keep `card_error` and its `/peers/<nick>/card_error` mirror in step
    /// with the outcome. A failure is retried on the ticks that the backoff allows (see
    /// `on_tick`).
    async fn sync_own_card(&mut self, state: &mut EventLoopState, context: &HandlerCtx<'_>) {
        let failure: Option<String> = publish_own_card(state, context).await.err().map(|error| {
            // The reason is read by peers: one line, and no more than a line holds.
            let reason: String = error.to_string().chars().take(200).collect();
            reason
        });
        if failure.is_some() {
            self.card_retry_wait = (self.card_retry_wait.max(1) * 2).min(CARD_RETRY_MAX_TICKS);
            self.card_retry_in = self.card_retry_wait;
        } else {
            self.card_retry_wait = 0;
            self.card_retry_in = 0;
        }
        if failure != self.card_error {
            let merge = card::card_error_merge(context.author, failure.as_deref());
            let _ = merge_own_meta_entry(state, context, merge, false).await;
            self.card_error = failure;
        }
    }
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
///
/// # Errors
/// The merge was refused, for example because its frame is over the size limit.
async fn merge_own_meta_entry(
    state: &mut EventLoopState,
    context: &HandlerCtx<'_>,
    merge: serde_json::Value,
    farewell: bool,
) -> anyhow::Result<()> {
    match broadcast_state_merge(
        state,
        // Daemon plumbing: the agent didn't write this, so don't surface it
        // as a "you changed shared state" event (nor race a fetch long-poll).
        StateMergeParams::new(MergeCtx::from(context), Channel::Meta, merge).with_surface(false),
    )
    .await
    {
        Ok(Some(bytes)) if farewell => {
            habilis_network::ops::unicast_farewell(state, &bytes);
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(error) => {
            tracing::warn!(%error, "own /peers entry merge failed");
            Err(anyhow::Error::from(error))
        }
    }
}

/// A peer's `working` with no text on a task already `working` changes nothing
/// the agent can act on. A 0.8/0.9 worker still re-sends one every minute to
/// keep the task alive, and each one woke the initiator's bell, so it is kept
/// off the surface like a beat. It still reaches the state machine, which
/// counts it as the peer's activity. A leg that sets or clears the waiting
/// reason (`prior_waiting` is the record's current one) is news and surfaces.
fn is_empty_working_repeat(
    prior: Option<super::TaskState>,
    prior_waiting: Option<&str>,
    message: &Authored,
) -> bool {
    use super::TaskState;
    prior == Some(TaskState::Working)
        && status_update_params::status_payload(message).is_ok_and(|payload| {
            !status_update_params::is_beat(&payload)
                && payload.status.state == TaskState::Working
                && status_update_params::status_waiting(&payload).as_deref() == prior_waiting
        })
        && status_update_params::task_text(message).trim().is_empty()
}

/// Whether the local agent is reading: `(bell armed, seconds since its last
/// poll)`. One source for `mesh/reader.get` and for the reader keys of our beats.
pub(crate) fn reader_liveness(app: &A2aApp, context: &HandlerCtx<'_>) -> (bool, Option<u64>) {
    // A CLI bell holds the OS lock. A parked cursor-less long fetch in process
    // is a bell too. An MCP fetch always passes a cursor, so it is a read only.
    let bell = bell::is_armed(context.mesh.as_ref(), context.author.as_str())
        || app.surfaced.has_parked_bell();
    (
        bell,
        app.surfaced.last_poll_secs_ago(tokio::time::Instant::now()),
    )
}

/// Whether a surfaced event should wake a parked `poll --long` bell: anything
/// the agent must see (`is_visible`) or act on (a task interaction). State and
/// meta document echoes, `fork`, and presence `alive` beats stay in the ring —
/// consumable in the next batch — but never ring the bell on their own, so a
/// self meta report at session start cannot cost the agent a wake-up turn.
/// The agent's own task echoes are quiet for the same reason: it already holds
/// the id of every leg it sent, and an echo landing right after a re-arm rang
/// the bell for nothing and made the Stop hook refuse the turn.
pub(crate) fn wakes(event: &output::OutputEvent) -> bool {
    use output::OutputEvent;
    if is_self_task_echo(event) {
        return false;
    }
    output::simple_event::is_visible(event)
        || matches!(
            event,
            OutputEvent::Task { .. }
                | OutputEvent::TaskMessage { .. }
                | OutputEvent::TaskTimeout { .. }
                | OutputEvent::TaskStalled { .. }
        )
}

fn is_self_task_echo(event: &output::OutputEvent) -> bool {
    matches!(
        event,
        output::OutputEvent::Task { is_self: true, .. }
            | output::OutputEvent::TaskMessage { is_self: true, .. }
    )
}

/// Whether a surfaced event belongs in the `poll`/`fetch` history — an explicit
/// allow-list of the documented pollable contract (chat, presence joined/left,
/// content task legs, and the transient `ping_report`/`peer_timeout`/
/// `peer_return`/`task_timeout`/`fork`). Deliberately an allow-list, not
/// "everything except X": operational notices (`info`/`error`/`msg_posted`) and
/// startup events (`ready`/`mesh_id`) also flow through the same `Output` tap,
/// and must NOT enter the ring. The `task` `Progress` beat is excluded too (a
/// liveness widget update, never a retained record).
pub(crate) fn is_pollable(event: &output::OutputEvent) -> bool {
    use output::OutputEvent;
    match event {
        OutputEvent::Message { .. }
        | OutputEvent::Presence { .. }
        | OutputEvent::PingReport { .. }
        | OutputEvent::PeerTimeout { .. }
        | OutputEvent::PeerReturn { .. }
        | OutputEvent::TaskTimeout { .. }
        | OutputEvent::TaskStalled { .. }
        | OutputEvent::TaskMessage { .. }
        | OutputEvent::Receipt { .. }
        | OutputEvent::StateChanged { .. }
        | OutputEvent::Fork { .. } => true,
        // A task liveness beat (a `Progress` widget update) is never retained;
        // the per-frame classification is the source of truth for it.
        OutputEvent::Task { message, .. } => !classify(message).beat(),
        OutputEvent::Info { .. }
        | OutputEvent::Error { .. }
        | OutputEvent::MsgPosted { .. }
        | OutputEvent::Ready { .. } => false,
    }
}

/// Per-tag/payload classification of an inbound a2a `App` frame — see
/// [`AppClass`]. Encodes in one place the wire policy the engine used to derive
/// from hardcoded tag checks.
fn classify(message: &Authored) -> AppClass {
    let tag = message.kind.app_tag().map(AppTag::as_str);
    // App payloads log (chat/status/artifact) only. RPC request/response frames
    // are plumbing: the result reaches the requester via its parked waiter,
    // never the chat log / poll-fetch buffer. A tag outside the a2a taxonomy is
    // not loggable (see `valid` — it never surfaces at all).
    let loggable = matches!(
        tag,
        Some(wire::BROADCAST | wire::MSG | wire::STATUS | wire::ARTIFACT)
    );
    // A task liveness beat: a status update marked `mesh:beat`.
    let beat = message.kind.is_app(wire::STATUS)
        && status_update_params::status_payload(message)
            .is_ok_and(|payload| status_update_params::is_beat(&payload));
    // The A2A boundary gate for a logical frame: a chat/task Message, status, or
    // artifact must parse and agree with its frame *and* carry the addressing its
    // tag mandates. `a2a_broadcast` is broadcast-only (`to: None`); `a2a_msg`,
    // status/artifact and RPC req/resp are directed-only (`to: Some`). Enforcing
    // the addressing here is what stops a forged broadcast status/artifact
    // (`to: None`) — which would otherwise pass, then retain + inbound-push on
    // every peer — and, for the two chat tags, it is the whole privacy boundary:
    // a directed `a2a_broadcast` would be a private line dressed as gossip-wide,
    // and a broadcast `a2a_msg` would be a msg delivered to everyone. A tag
    // outside the a2a taxonomy is invalid: a signed frame with an arbitrary tag
    // must not cross to api consumers, so drop it whole. `None` is a non-App
    // frame the engine gates itself.
    let directed = matches!(message.kind, MessageKind::App { to: Some(_), .. });
    let valid: bool = match tag {
        Some(wire::BROADCAST) => {
            !directed && status_update_params::broadcast_payload(message).is_ok()
        }
        Some(wire::MSG) => directed && status_update_params::msg_payload(message).is_ok(),
        Some(wire::STATUS) => directed && status_update_params::status_payload(message).is_ok(),
        Some(wire::ARTIFACT) => directed && status_update_params::artifact_payload(message).is_ok(),
        Some(wire::REQ | wire::RESP) => directed,
        Some(wire::RECEIPT) => directed && parse_receipt(message).is_some(),
        None => true,
        Some(_) => false,
    };
    // Only broadcast chat carries `seq`/parents and so participates in
    // fork-detection + the cross-author DAG. A msg must NOT be chained, and not
    // merely because the chain is for broadcast: a directed frame reaches only
    // its addressee, so a chain carried on msgs is unobservable to everyone else
    // and every non-addressee would see permanent gaps in that author's chain —
    // which is precisely what fork detection reads as a fork.
    let chained = message.kind.is_app(wire::BROADCAST);
    // Every directed tag — msg, worker status/artifact push, RPC
    // request/response — arrives sealed (encrypted) to its addressee, so a
    // directed frame addressed to us must unseal-or-drop. Sealing is what keeps
    // a relayed msg unreadable by the peers forwarding it. Broadcast chat is
    // public by definition, and an unknown tag is never sealed.
    let sealed = matches!(
        tag,
        Some(wire::MSG | wire::STATUS | wire::ARTIFACT | wire::REQ | wire::RESP | wire::RECEIPT)
    );
    let content = AppClass::NONE
        .with_if(AppClass::LOGGABLE, loggable)
        .with_if(AppClass::BEAT, beat)
        .with_if(AppClass::VALID, valid);
    content
        .with_if(AppClass::CHAINED, chained)
        .with_if(AppClass::SEALED, sealed)
}

/// Route a content frame — chat, or one of the three task shapes — to its
/// lifecycle handler. `loggable` is false for a directed frame addressed
/// elsewhere or a liveness beat. `fresh_for_us` is true for a directed msg or
/// task leg that reached us for the first time, inside our join horizon: the
/// only frames that earn a delivery receipt. A frame from before the horizon is
/// never surfaced, so a receipt for it would claim a delivery nobody sees.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "foreign non_exhaustive enum: the compiler needs a wildcard arm; every frame that no arm above names is a no-op"
)]
fn route_content(
    message: &Authored,
    surfaceable: bool,
    app: &mut A2aApp,
    context: &HandlerCtx<'_>,
) -> Routed {
    match &message.kind {
        // Each chat tag is pinned to its addressing here as well as in
        // `classify`: a directed BROADCAST or a broadcast MSG falls through to
        // the no-op arm rather than being surfaced/logged by a relay.
        MessageKind::App { tag, to: None, .. } if tag.as_str() == wire::BROADCAST => Routed {
            loggable: surface_chat(message, surfaceable, app, |out, surface| {
                handle_broadcast(out, message, surface)
            }),
            fresh_for_us: false,
        },
        MessageKind::App {
            tag, to: Some(to), ..
        } if tag.as_str() == wire::MSG => {
            let fresh_for_us = surfaceable
                && to == context.author
                && !is_replayed_chat(&app.surfaced_chat, message);
            let loggable = surface_chat(message, surfaceable, app, |out, surface| {
                handle_msg(
                    out,
                    MsgParams {
                        message,
                        to,
                        surfaceable: surface,
                        self_author: context.author,
                    },
                )
            });
            Routed {
                loggable,
                fresh_for_us,
            }
        }
        MessageKind::App {
            tag, to: Some(to), ..
        } if tag.as_str() == wire::STATUS || tag.as_str() == wire::ARTIFACT => {
            let leg = handle_task_leg(
                TaskLegParams {
                    message,
                    to,
                    surfaceable,
                },
                app,
                context,
            );
            Routed {
                loggable: leg.loggable,
                fresh_for_us: leg.fresh_for_us,
            }
        }
        MessageKind::App {
            tag, to: Some(to), ..
        } if tag.as_str() == wire::RECEIPT => {
            if to == context.author {
                surface_receipt(message, app);
            }
            Routed::default()
        }
        _ => Routed::default(),
    }
}

#[derive(Default)]
struct Routed {
    loggable: bool,
    fresh_for_us: bool,
}

fn parse_receipt(message: &Authored) -> Option<Receipt> {
    serde_json::from_str(message.body.as_str()).ok()
}

/// A receipt we did not author reaches this hook only: the engine drops our own
/// frames by pubkey before it, and `route_content` checks the addressee.
fn surface_receipt(message: &Authored, app: &A2aApp) {
    let Some(receipt) = parse_receipt(message) else {
        return;
    };
    if !app
        .sent_refs
        .contains(&(receipt.ref_id.clone(), message.author.clone()))
    {
        tracing::debug!(
            target: "agent_gossip::a2a",
            author = %message.author,
            "receipt for a frame we did not send to its author dropped"
        );
        return;
    }
    let out = &app.output;
    let state = match receipt.state {
        ReceiptState::Delivered => "delivered",
        ReceiptState::Read => "read",
    };
    out.receipt(output::ReceiptParams {
        from: &message.author,
        ref_id: &receipt.ref_id,
        state,
        ts: u64::try_from(message.timestamp.as_secs()).unwrap_or_default(),
    });
}

/// The value cluster [`handle_task_leg`] needs beyond its `app`/`ctx` handles
/// — the input of [`route_content`]'s task-leg arm.
#[derive(Clone, Copy)]
struct TaskLegParams<'caller> {
    message: &'caller Authored,
    to: &'caller Nickname,
    surfaceable: bool,
}

/// Process an inbound task leg (a task-related `a2a_status` or `a2a_artifact`)
/// for its addressee: advance the coarse state machine (addressee only — a
/// third-party relay tracks nothing), then surface it via the lifecycle layer.
fn handle_task_leg(leg: TaskLegParams<'_>, app: &mut A2aApp, context: &HandlerCtx<'_>) -> Routed {
    let TaskLegParams {
        message,
        to,
        surfaceable,
    } = leg;
    let mut surface = surfaceable;
    let mut replay = false;
    // Only the addressee is a party: a third-party relay tracks nothing. The
    // receiver's body is already unsealed here, so the task id parses out of it.
    if to == context.author
        && let Some(task_id) = status_update_params::frame_task_id(message)
    {
        // …and being the addressee is not enough: the leg must come from the
        // task's own counterparty. Anyone can seal to us (the key is published
        // in the meta document) and name a task id, so without this a bystander
        // both drives the record and — the part the state machine cannot
        // defend — has its text surfaced to our skill *as the worker's result*.
        // Dropping it here covers the surface as well as the machine.
        if app
            .tasks
            .get(&task_id)
            .is_some_and(|rec| rec.peer != message.author)
        {
            return Routed::default();
        }
        let prior = app.tasks.get(&task_id).map(|rec| rec.state);
        let prior_waiting = app
            .tasks
            .get(&task_id)
            .and_then(|rec| rec.waiting.as_deref());
        surface &= !is_empty_working_repeat(prior, prior_waiting, message);
        if task_record::is_replay(&app.tasks, &app.closed_tasks, &task_id, &message.id) {
            tracing::info!(
                target: "agent_gossip::a2a",
                %task_id,
                leg = %message.id,
                author = %message.author,
                "replayed task leg kept off the surface"
            );
            surface = false;
            replay = true;
        }
        let now = Instant::now();
        let reader = task_record::ingest(
            &mut app.tasks,
            task_record::IngestLegParams {
                frame: message,
                task_id: &task_id,
                mine: false,
                now,
            },
        );
        if let Some(reader) = reader
            && let Some(notice) = task_record::stall_notice(&mut app.tasks, &task_id, reader, now)
        {
            app.output.task_stalled(&notice);
        }
        if surface {
            task_record::note_surfaced(&mut app.tasks, &task_id, message);
        }
    }
    let loggable = handle_task(
        &app.output,
        TaskDisplayParams {
            message,
            to,
            surfaceable: surface,
            self_author: context.author,
        },
    );
    // A beat is liveness plumbing and never logged, so it earns no receipt.
    Routed {
        loggable,
        fresh_for_us: surfaceable && to == context.author && loggable && !replay,
    }
}

/// Run a chat handler with a replayed line kept off the surface, and remember
/// the line once it is printed. A sharded body reaches here with its group id
/// as the logical id, the same on every completion, so a group the engine
/// completes again is covered too. Loggability stays the handler's answer: a
/// line we refuse to log is one every peer's digest re-offers.
fn surface_chat(
    message: &Authored,
    surfaceable: bool,
    app: &mut A2aApp,
    handler: impl FnOnce(&output::Output, bool) -> bool,
) -> bool {
    let replay = is_replayed_chat(&app.surfaced_chat, message);
    if replay && surfaceable {
        tracing::info!(
            target: "agent_gossip::a2a",
            id = %message.id,
            author = %message.author,
            "replayed chat message kept off the surface"
        );
    }
    let surface = surfaceable && !replay;
    let logged = handler(&app.output, surface);
    if surface && logged {
        app.surfaced_chat.insert(message.dedup_key());
    }
    logged
}

fn is_replayed_chat(surfaced: &BoundedFifoSet<DedupKey>, message: &Authored) -> bool {
    surfaced.contains(&message.dedup_key())
}

/// Returns whether the message should be logged (pushed to the poll buffer).
/// `a2a_broadcast` is mesh broadcast chat — always loggable. `surfaceable`
/// gates only the *display*: a pre-join message is still logged/relayed but not
/// printed.
fn handle_broadcast(out: &output::Output, message: &Authored, surfaceable: bool) -> bool {
    if message.kind.is_app(wire::BROADCAST) {
        if surfaceable {
            out.print_message(message);
        }
        true
    } else {
        false
    }
}

/// The value cluster [`handle_msg`] needs beyond its `out` sink handle.
#[derive(Clone, Copy)]
struct MsgParams<'caller> {
    message: &'caller Authored,
    to: &'caller Nickname,
    surfaceable: bool,
    self_author: &'caller Nickname,
}

/// A msg: printed and logged **only by its addressee** (`to == self_author`).
/// Anyone else is a relay: it forwards the frame and returns `false` here, so
/// the msg never reaches that peer's transcript or poll buffer.
///
/// The party check is not redundant with unicast routing or sealing. Unicast
/// decides who receives the bytes and sealing decides who can read them, but a
/// multihop relay legitimately holds a msg it is not party to; this is the gate
/// that stops it surfacing there. `surfaceable` gates only the *display*
/// (join-horizon), never the relay/log.
///
/// The sender's own copy does **not** come through here — it is rendered from a
/// plaintext twin on the send path (`send::print_echo`), because the wire body
/// is sealed to the addressee. Both call sites are inbound hooks, and the
/// engine drops our own frames by **pubkey** before either one, so a frame
/// reaching this function carrying our nickname is by construction someone
/// else's: nicknames are display labels the engine deliberately never pins.
/// An `author == self_author` escape hatch here therefore admitted nothing
/// honest and exactly one thing hostile — a relayed msg between two other
/// peers, surfaced to us as though we had sent it.
fn handle_msg(out: &output::Output, params: MsgParams<'_>) -> bool {
    let MsgParams {
        message,
        to,
        surfaceable,
        self_author,
    } = params;
    if to != self_author {
        return false;
    }
    if surfaceable {
        out.print_message(message);
    }
    true
}

/// The value cluster [`handle_task`] needs beyond its `out` sink handle.
#[derive(Clone, Copy)]
struct TaskDisplayParams<'caller> {
    message: &'caller Authored,
    to: &'caller Nickname,
    surfaceable: bool,
    self_author: &'caller Nickname,
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
    !status_update_params::status_payload(message)
        .is_ok_and(|payload| status_update_params::is_beat(&payload))
}

/// Route an A2A RPC frame: a request addressed to us is served (and a response
/// broadcast back); a response to one of our calls resolves its waiter (matched
/// by `corr` *and* the responding peer). Frames addressed to another peer are
/// relayed only. Plumbing — never logged/retained.
async fn handle_a2a_rpc(
    message: &Authored,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    context: &HandlerCtx<'_>,
) {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "foreign non_exhaustive enum: the compiler needs a wildcard arm; a frame that is not a directed, correlated request or response is not ours"
    )]
    let (tag, corr) = match &message.kind {
        MessageKind::App {
            tag,
            to: Some(to),
            corr: Some(corr),
        } if to == context.author => (tag.as_str(), corr),
        _ => return,
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
            context,
        )
        .await;
    } else if tag == wire::RESP && app.has_a2a_waiter(corr, &message.author) {
        // Only act on a response to a call WE actually issued (a matching
        // outstanding waiter). An unsolicited response is ignored — otherwise
        // any member could inject phantom initiator-side task records by forging
        // responses. The waiter adopts the authoritative `Task` a `SendMessage`
        // returned.
        app.fulfill_a2a_waiter(corr, &message.author, message.body.as_str());
    }
}

/// One inbound `a2a_req` frame's request body plus the correlation id its
/// `a2a_resp` must echo back — grouped so [`handle_a2a_req`] stays within the
/// argument budget alongside its `state`/`app`/`ctx` handles.
struct A2aReqParams<'caller> {
    message: &'caller Authored,
    corr: &'caller habilis_network::protocol::CorrId,
}

/// Serve one inbound `a2a_req` addressed to us: classify the JSON-RPC request
/// against the safe method set (`gossip_rpc::classify`), run the resulting
/// action, and broadcast an `a2a_resp` back to the requester echoing `corr`.
async fn handle_a2a_req(
    request: A2aReqParams<'_>,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    context: &HandlerCtx<'_>,
) {
    use crate::a2a::served::Served;
    let A2aReqParams { message, corr } = request;
    let requester = message.author.clone();
    // Parse the JSON-RPC envelope; a malformed request is a parse-error reply.
    let outcome: Result<serde_json::Value, RpcError> =
        match serde_json::from_str::<serde_json::Value>(message.body.as_str()) {
            Ok(envelope) => {
                let method = envelope["method"].as_str().unwrap_or_default();
                match served::classify(method, &envelope["params"], &requester, app) {
                    Served::Op(op) => {
                        let out = app.output.clone();
                        rpc_error::handle_op(
                            rpc_error::HandleOpParams {
                                op,
                                mesh: context.mesh,
                                author: context.author,
                                our_pubkey: context.our_pubkey,
                                // No local HTTP port on the gossip path; only card
                                // ops read it, and those aren't in the safe set here.
                                a2a_port: 0,
                                app,
                            },
                            state,
                            context.sender,
                            &out,
                        )
                        .await
                    }
                    Served::Ingest(payload) => {
                        ingest_remote_message(&payload, &requester, app, context)
                    }
                    Served::ShardRepair { group, missing } => {
                        resend_cached_shards(
                            ShardRepairParams {
                                group: &group,
                                missing: &missing,
                                requester: &requester,
                            },
                            state,
                            context,
                        )
                        .await
                    }
                    Served::ReaderGet => {
                        let (bell, last_poll_secs_ago) = reader_liveness(app, context);
                        Ok(serde_json::json!({
                            "bell": bell,
                            "last_poll_secs_ago": last_poll_secs_ago,
                        }))
                    }
                    Served::Reject(error) => Err(error),
                }
            }
            Err(error) => Err(RpcError::invalid_params(&format!(
                "malformed JSON-RPC request: {error}"
            ))),
        };
    let response = match outcome {
        Ok(result) => serde_json::json!({ "result": result }),
        Err(error) => {
            serde_json::json!({ "error": { "code": error.code, "message": error.message } })
        }
    };
    let Ok(body) = MessageBody::try_new(response.to_string()) else {
        tracing::warn!("a2a rpc response body invalid");
        return;
    };
    // Seal the response to the requester — a relay forwards it but cannot read it.
    let body = match broadcast_params::seal_directed(state, &requester, &body) {
        Ok(sealed) => sealed,
        Err(error) => {
            tracing::warn!(%error, "cannot seal a2a rpc response — dropping");
            return;
        }
    };
    let kind = MessageKind::App {
        tag: wire::app_tag(wire::RESP),
        to: Some(requester),
        corr: Some(corr.clone()),
    };
    // Directed reply over the shared RPC sender: unicast, transparently
    // splitting a large sealed response into shard frames.
    if let Err(error) = broadcast_params::send_directed_rpc(
        broadcast_params::DirectedRpcParams {
            mesh: context.mesh,
            author: context.author,
            kind,
            body,
        },
        state,
        context.sender,
    )
    .await
    {
        tracing::warn!(%error, "a2a rpc response send failed");
    }
}

/// The value cluster [`resend_cached_shards`] needs beyond its `state`/`ctx`
/// handles.
struct ShardRepairParams<'caller> {
    group: &'caller habilis_network::protocol::ShardGroup,
    missing: &'caller [u32],
    requester: &'caller Nickname,
}

/// Serve a `shard/repair` ask: re-deliver the named cached frames of one of our
/// big (unlogged) outbound groups to the requester. The frames are the raw,
/// already-signed shards we cached at send time (`state.shard_cache_mut()`), so they
/// reassemble on the requester exactly as the original send would have.
async fn resend_cached_shards(
    params: ShardRepairParams<'_>,
    state: &mut EventLoopState,
    context: &HandlerCtx<'_>,
) -> Result<serde_json::Value, RpcError> {
    let ShardRepairParams {
        group,
        missing,
        requester,
    } = params;
    let frames: Vec<_> = state.shard_cache_mut().frames(group, missing).collect();
    let mut resent = 0usize;
    for bytes in frames {
        let parsed = Message::parse(bytes.as_bytes()).ok();
        let Some(message) = parsed.and_then(|message| message.verify().ok()) else {
            continue; // never: the cache holds frames we serialized ourselves
        };
        if let Some(to) = habilis_network::protocol::sole_addressee(&message.kind)
            && to != requester
        {
            continue;
        }
        // In the background: this serves a gossip RPC inline on the event
        // loop, and the requester's connection may be cold. A shard that does
        // not start is asked for again on the requester's next tick.
        if habilis_network::ops::deliver_in_background(&message, bytes, state, context.sender).await
        {
            resent += 1;
        }
    }
    tracing::debug!(
        target: "agent_gossip::a2a",
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
/// Refusal for an offer the registry has no room for, or `None` to admit.
fn task_admission_error(app: &A2aApp, requester: &Nickname) -> Option<RpcError> {
    use crate::a2a::task_record::Admission;
    use crate::a2a::tuning::{TASKS_CAP, TASKS_PER_PEER_CAP};
    // A2A has no "too many tasks" error, so this is an application code in the
    // implementation-defined server range, alongside the party check's -32003.
    let refuse = |message: String| {
        Some(RpcError {
            code: -32004,
            message,
        })
    };
    match task_record::admit_new_task(&app.tasks, requester) {
        Admission::Ok => None,
        Admission::RegistryFull => refuse(format!(
            "this peer is holding its maximum of {TASKS_CAP} tasks"
        )),
        Admission::PeerAtCap => refuse(format!(
            "you already hold {TASKS_PER_PEER_CAP} live tasks with this peer"
        )),
    }
}

fn ingest_remote_message(
    payload: &crate::a2a::Message,
    requester: &Nickname,
    app: &mut A2aApp,
    context: &HandlerCtx<'_>,
) -> Result<serde_json::Value, RpcError> {
    use crate::a2a::task_record::{LegInfo, LegKind};
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
                return Err(RpcError::invalid_params(
                    "SendMessage names a taskId with no task on this peer",
                ));
            }
            Some(rec) if &rec.peer != requester => {
                return Err(RpcError {
                    code: -32003,
                    message: "not a party to the task named by taskId".to_string(),
                });
            }
            Some(_) => {}
        }
    } else if let Some(error) = task_admission_error(app, requester) {
        // An offer (no taskId) is the one registry entry a remote peer mints
        // directly. Refusing over the quota keeps `app.tasks` bounded and,
        // just as importantly, keeps the sweep's per-expiry work bounded: it
        // awaits a sealed unicast per expired task while holding the event
        // loop, so an unbounded registry is a stall as well as a leak.
        return Err(error);
    }
    task_record::apply(
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
    let text = status_update_params::display_text(payload);
    let label = status_update_params::task_label(payload);
    let mesh_name: &str = context.mesh.as_str();
    let peer_name: &str = context.author.as_str();
    app.output.task_message(&output::TaskMessageLeg {
        id: payload.message_id.as_str(),
        mesh: mesh_name,
        author: requester.as_str(),
        peer: peer_name,
        task_id: task_id.as_str(),
        state: rec_state,
        text: &text,
        label: label.as_deref(),
        message: Some(payload),
        is_self: false,
    });
    // A2A v1.0 `SendMessage` returns a `SendMessageResponse` oneof — creation
    // yields `{"task": <Task>}`.
    app.tasks
        .get(&task_id)
        .map(|rec| serde_json::json!({ "task": rpc_error::task_object(&task_id, rec, context.mesh) }))
        .ok_or_else(|| RpcError::internal("task record missing after ingest"))
}

/// Adopt `author`'s published endpoint hint (`/peers/<nick>/card/endpoint`) into
/// the dial book — the meta-doc counterpart to `handle_peer_info`. Binds
/// `peer_endpoints[author] = eid` and seeds iroh's address book via
/// [`add_peer_addr`], so a directed unicast can reach a peer we learned about
/// from the synced meta doc even before its gossiped `PeerInfo` arrives. Both
/// are last-writer-wins / additive, so this never conflicts with `PeerInfo`. It
/// does **not** graft a gossip link — mesh formation stays `PeerInfo`'s job.
fn adopt_meta_endpoint(author: &Nickname, state: &mut EventLoopState, context: &HandlerCtx<'_>) {
    let doc = state.doc(Channel::Meta).to_json();
    let Some((peer_id, peer_addr)) = card::peer_endpoint(&doc, author) else {
        return;
    };
    // Never bind our own endpoint or the rendezvous pseudo-node.
    if peer_id == context.endpoint.id() || peer_id == context.rendezvous_id {
        return;
    }
    state.note_peer_endpoint(author.clone(), peer_addr.clone());
    add_peer_addr(state.address_book(), peer_addr);
}

#[cfg(test)]
mod classify_tests {
    use habilis_network::protocol::message::Authored;
    use habilis_network::protocol::{
        AppFrameParams, MeshId, Message, MessageBody, MessageId, MessageKind, Nickname,
    };
    use habilis_network::util::bounded_fifo_set::BoundedFifoSet;

    use super::{classify, is_empty_working_repeat, is_replayed_chat, surface_chat, wakes};
    use crate::a2a::a2a_app::A2aApp;
    use crate::a2a::{TaskState, status_update_params, wire};
    use crate::output::OutputEvent;

    fn mesh() -> MeshId {
        MeshId::from("test")
    }

    /// The author's view of a test frame: signed by a throwaway key, then opened as a view.
    fn view(message: Message) -> Authored {
        message
            .sign(&habilis_network::protocol::Identity::generate())
            .authored()
    }

    fn app_frame_raw(tag: &str) -> Message {
        Message::new_app(
            &mesh(),
            &Nickname::from("author"),
            AppFrameParams::new(wire::app_tag(tag), None, None, MessageBody::from("{}")),
        )
    }

    fn app_frame(tag: &str) -> Authored {
        view(app_frame_raw(tag))
    }

    // A well-formed broadcast chat frame: its frame id equals the payload's
    // messageId, so `broadcast_payload` accepts it — the `to` is the only
    // variable.
    fn chat_frame_raw(to: Option<Nickname>) -> Message {
        let sw = mesh();
        let payload = status_update_params::compose_broadcast(&sw, "hi");
        let id = MessageId::try_new(payload.message_id.as_str()).unwrap();
        let body = status_update_params::payload_body(&payload).unwrap();
        Message::new_app(
            &sw,
            &Nickname::from("author"),
            AppFrameParams::new(wire::app_tag(wire::BROADCAST), to, None, body),
        )
        .with_id(id)
    }

    fn chat_frame(to: Option<Nickname>) -> Authored {
        view(chat_frame_raw(to))
    }

    // The directed twin of [`chat_frame`]: a well-formed msg, `to` the variable.
    fn msg_frame_raw(to: Option<Nickname>) -> Message {
        let sw = mesh();
        let payload = status_update_params::compose_msg(&sw, "hi");
        let id = MessageId::try_new(payload.message_id.as_str()).unwrap();
        let body = status_update_params::payload_body(&payload).unwrap();
        Message::new_app(
            &sw,
            &Nickname::from("author"),
            AppFrameParams::new(wire::app_tag(wire::MSG), to, None, body),
        )
        .with_id(id)
    }

    fn msg_frame(to: Option<Nickname>) -> Authored {
        view(msg_frame_raw(to))
    }

    // A well-formed task-status frame (its payload's contextId names the mesh).
    fn status_frame(to: Option<Nickname>) -> Authored {
        status_frame_in(TaskState::Working, to)
    }

    fn status_frame_in_raw(state: TaskState, to: Option<Nickname>) -> Message {
        status_update_params::test_status_frame(&mesh(), state, to)
    }

    fn status_frame_in(state: TaskState, to: Option<Nickname>) -> Authored {
        view(status_frame_in_raw(state, to))
    }

    fn task_event(message: Authored, is_self: bool) -> OutputEvent {
        OutputEvent::Task {
            message: Box::new(message),
            is_self,
        }
    }

    fn task_message(is_self: bool) -> OutputEvent {
        OutputEvent::TaskMessage {
            id: String::from("m"),
            mesh: String::from("sw"),
            author: String::from("initiator"),
            peer: String::from("worker"),
            task_id: String::from("t"),
            state: Some(TaskState::Working),
            text: String::from("approved"),
            label: None,
            message: None,
            is_self,
        }
    }

    // The agent already knows every task leg it sent: the CLI returned its id.
    // Its echo landed right after the re-arm and rang the bell for nothing,
    // which made the Stop hook refuse the turn. The peer's legs still wake.
    #[test]
    fn no_self_task_echo_wakes_the_bell() {
        let to = Some(Nickname::from("initiator"));
        for state in [
            TaskState::Working,
            TaskState::InputRequired,
            TaskState::Completed,
            TaskState::Failed,
        ] {
            let own = task_event(status_frame_in(state, to.clone()), true);
            assert!(!wakes(&own), "own {state:?} echo is quiet");
            let peer = task_event(status_frame_in(state, to.clone()), false);
            assert!(wakes(&peer), "a peer's {state:?} status still wakes");
        }

        assert!(
            !wakes(&task_event(app_frame(wire::ARTIFACT), true)),
            "own artifact echo is quiet"
        );
        assert!(
            wakes(&task_event(app_frame(wire::ARTIFACT), false)),
            "a peer's artifact still wakes"
        );
        assert!(
            !wakes(&task_message(true)),
            "own task message echo is quiet"
        );
        assert!(
            wakes(&task_message(false)),
            "a peer's task message still wakes"
        );
    }

    fn receipt_event() -> OutputEvent {
        OutputEvent::Receipt {
            from: Nickname::from("peer"),
            ref_id: "abc".to_owned(),
            state: "delivered".to_owned(),
            ts: 1,
        }
    }

    #[test]
    fn a_receipt_is_pollable_but_never_wakes_the_bell() {
        assert!(
            super::is_pollable(&receipt_event()),
            "a receipt is not pollable"
        );
        assert!(!wakes(&receipt_event()), "a receipt does not wake the bell");
    }

    fn receipt_frame_raw(to: Option<Nickname>, body: &str) -> Message {
        Message::new_app(
            &mesh(),
            &Nickname::from("author"),
            AppFrameParams::new(
                wire::app_tag(wire::RECEIPT),
                to,
                None,
                MessageBody::from(body),
            ),
        )
    }

    fn receipt_frame(to: Option<Nickname>, body: &str) -> Authored {
        view(receipt_frame_raw(to, body))
    }

    #[test]
    fn a_receipt_is_valid_only_when_directed_with_a_parseable_body() {
        let body = r#"{"ref":"abc","state":"delivered"}"#;
        let directed = classify(&receipt_frame(Some(Nickname::from("peer")), body));
        assert!(
            directed.valid() && directed.sealed() && !directed.loggable(),
            "a directed chat is valid, sealed and not loggable"
        );
        assert!(!classify(&receipt_frame(None, body)).valid(), "broadcast");
        assert!(
            !classify(&receipt_frame(Some(Nickname::from("peer")), "{}")).valid(),
            "no ref or state"
        );
    }

    fn working_with_raw(note: Option<&str>, metadata: Option<serde_json::Value>) -> Message {
        let status = status_update_params::status_update(
            &mesh(),
            status_update_params::StatusUpdateParams {
                task_id: &crate::a2a::TaskId::random(),
                state: TaskState::Working,
                note,
                metadata,
            },
        );
        Message::new_app(
            &mesh(),
            &Nickname::from("author"),
            AppFrameParams::new(
                wire::app_tag(wire::STATUS),
                Some(Nickname::from("initiator")),
                None,
                status_update_params::payload_body(&status).unwrap(),
            ),
        )
    }

    fn working_with(note: Option<&str>, metadata: Option<serde_json::Value>) -> Authored {
        view(working_with_raw(note, metadata))
    }

    // Only an empty `working` on a task already `working` is a repeat: the
    // accept, a progress note, a beat and every other state still surface.
    #[test]
    fn only_an_empty_working_on_a_working_task_is_a_repeat() {
        let working = Some(TaskState::Working);
        assert!(
            is_empty_working_repeat(working, None, &working_with(None, None)),
            "a repeated working status is recognized"
        );
        assert!(
            !is_empty_working_repeat(Some(TaskState::Submitted), None, &working_with(None, None)),
            "the accept surfaces"
        );
        assert!(
            !is_empty_working_repeat(working, None, &working_with(Some("tests pass"), None)),
            "a progress note surfaces"
        );
        let beat = working_with(None, Some(status_update_params::beat_metadata(None)));
        assert!(
            !is_empty_working_repeat(working, None, &beat),
            "a beat keeps its own path"
        );
        assert!(
            !is_empty_working_repeat(
                working,
                None,
                &status_frame_in(TaskState::InputRequired, None)
            ),
            "a question surfaces"
        );
    }

    /// The `--waiting` leg and the leg that clears it change what the initiator
    /// can act on, so each surfaces once. Pre-fix both were empty `working`
    /// repeats and stayed off the surface, so the initiator agent never saw
    /// that the worker waits on its user.
    #[test]
    fn a_waiting_leg_and_its_clear_each_surface_once() {
        let working = Some(TaskState::Working);
        let reason = "asks its user";
        let waiting = working_with(None, status_update_params::waiting_metadata(Some(reason)));
        assert!(
            !is_empty_working_repeat(working, None, &waiting),
            "the --waiting leg surfaces"
        );
        assert!(
            is_empty_working_repeat(working, Some(reason), &waiting),
            "the same reason again stays off the surface"
        );
        assert!(
            !is_empty_working_repeat(working, Some(reason), &working_with(None, None)),
            "the clear surfaces"
        );
        assert!(
            is_empty_working_repeat(working, None, &working_with(None, None)),
            "a plain repeat stays off the surface"
        );
    }

    fn chat_from(identity: &habilis_network::protocol::Identity) -> Authored {
        msg_frame_raw(None).sign(identity).authored()
    }

    // Anti-entropy re-sends a chat line once the engine has forgotten its key.
    // Pre-fix it surfaced again as a new visible line, up to hours late.
    #[test]
    fn a_chat_line_surfaces_once() {
        let mut surfaced = BoundedFifoSet::new(8);
        let chat = chat_from(&habilis_network::protocol::Identity::generate());
        surfaced.insert(chat.dedup_key());
        assert!(
            is_replayed_chat(&surfaced, &chat),
            "a surfaced chat is recognized as replayed"
        );
    }

    // The key is the engine's `(pubkey, id)`: another author reusing the id
    // (a signed forgery) must not hide the real line.
    #[test]
    fn the_same_id_from_another_author_still_surfaces() {
        let mut surfaced = BoundedFifoSet::new(8);
        let chat = chat_from(&habilis_network::protocol::Identity::generate());
        surfaced.insert(chat.dedup_key());
        let forgery = (*chat)
            .clone()
            .sign(&habilis_network::protocol::Identity::generate())
            .authored();
        assert!(
            !is_replayed_chat(&surfaced, &forgery),
            "a forged chat is not a replay"
        );
    }

    // `surface_chat` records a line only once it was really printed, and keeps
    // returning the handler's loggability for a replay: a line we refuse to
    // log is one every peer's digest re-offers.
    #[test]
    fn surface_chat_records_only_printed_lines_and_keeps_loggability() {
        let mut app = A2aApp::detached(crate::output::Output::silent());
        let chat = chat_from(&habilis_network::protocol::Identity::generate());

        // A msg addressed to someone else: the handler declines, nothing recorded.
        assert!(
            !surface_chat(&chat, true, &mut app, |_, _| false),
            "a chat is not surfaced when the gate refuses it"
        );
        assert!(
            !is_replayed_chat(&app.surfaced_chat, &chat),
            "a refused chat is not recorded"
        );

        // Before the join horizon: logged but not printed, nothing recorded.
        assert!(
            surface_chat(&chat, false, &mut app, |_, _| true),
            "a fresh chat surfaces"
        );
        assert!(
            !is_replayed_chat(&app.surfaced_chat, &chat),
            "a surfaced chat is recorded"
        );

        // Printed once, then recorded.
        assert!(
            surface_chat(&chat, true, &mut app, |_, surface| {
                assert!(surface, "a new line surfaces");
                true
            }),
            "a chat surfaces once the callback accepts it"
        );
        assert!(
            is_replayed_chat(&app.surfaced_chat, &chat),
            "a surfaced chat is recorded as a replay"
        );

        // The replay: kept off the surface, still logged.
        assert!(
            surface_chat(&chat, true, &mut app, |_, surface| {
                assert!(!surface, "a replay does not surface");
                true
            }),
            "a chat is not surfaced twice"
        );
    }

    // These assertions are deliberately worded "broadcast"/"directed" rather
    // than "chat": both tags are chat, so a `chained` that silently followed the
    // wrong one would still satisfy a test phrased in terms of chat.
    #[test]
    fn msg_is_loggable_but_rpc_is_not() {
        let message = classify(&app_frame(wire::BROADCAST));
        assert!(message.loggable(), "broadcast chat is loggable");
        assert!(
            message.chained(),
            "broadcast chat carries the per-author chain"
        );
        let request = classify(&app_frame(wire::REQ));
        assert!(
            !request.loggable(),
            "an RPC request is plumbing, not logged"
        );
        assert!(!request.chained(), "only broadcast chat is chained");
    }

    // A msg is retained like broadcast chat but must stay out of the hash chain:
    // it reaches only its addressee, so every other peer would see permanent
    // gaps in the author's chain — which fork detection reads as a fork.
    #[test]
    fn dm_is_loggable_and_sealed_but_never_chained() {
        let dm = classify(&msg_frame(Some(Nickname::from("bob"))));
        assert!(dm.loggable(), "a msg is retained");
        assert!(dm.sealed(), "a msg is sealed to its addressee");
        assert!(!dm.chained(), "a msg must not join the cross-author chain");
    }

    #[test]
    fn unknown_tag_is_invalid_and_not_loggable() {
        let cls = classify(&app_frame("admin"));
        assert!(!cls.valid(), "an unknown tag fails the boundary gate");
        assert!(!cls.loggable(), "an unknown tag is not logged");
        assert!(!cls.beat(), "an unknown tag is not a beat");
        assert!(!cls.chained(), "an unknown tag is not chained");
    }

    #[test]
    fn non_app_kinds_have_no_classification_effect() {
        let peer_info = classify(&view(Message::new_peer_info(
            &mesh(),
            &Nickname::from("author"),
            MessageBody::from("{}"),
        )));
        assert!(!peer_info.loggable(), "a peer info is not loggable");
        let _ = MessageKind::PeerInfo;
    }

    // ① A forged broadcast status (`to: None`) — even with a well-formed body —
    // must fail the boundary gate, so it is dropped before it can retain/push
    // on every peer. (`a2a_artifact` shares the directed-only rule.)
    #[test]
    fn broadcast_status_is_invalid() {
        let cls = classify(&status_frame(None));
        assert!(
            !cls.valid(),
            "a2a_status is directed-only; a `to: None` status is a forged broadcast"
        );
    }

    // ② Each chat tag is pinned to one addressing, and together these two are
    // the privacy boundary: a directed `a2a_broadcast` would be a private line
    // dressed as gossip-wide, and a broadcast `a2a_msg` would be a msg handed to
    // every peer.
    #[test]
    fn directed_broadcast_is_invalid() {
        let cls = classify(&chat_frame(Some(Nickname::from("bob"))));
        assert!(
            !cls.valid(),
            "a2a_broadcast is broadcast-only; a directed one must not cross the gate"
        );
    }

    #[test]
    fn broadcast_dm_is_invalid() {
        let cls = classify(&msg_frame(None));
        assert!(
            !cls.valid(),
            "a2a_msg is directed-only; a `to: None` msg would reach every peer"
        );
    }

    // A msg whose payload declares `mesh-broadcast` is a broadcast replayed under
    // the directed tag — the extension is a gate, not decoration.
    #[test]
    fn dm_declaring_the_broadcast_extension_is_invalid() {
        let sw = mesh();
        let payload = status_update_params::compose_broadcast(&sw, "hi");
        let id = MessageId::try_new(payload.message_id.as_str()).unwrap();
        let body = status_update_params::payload_body(&payload).unwrap();
        let frame = Message::new_app(
            &sw,
            &Nickname::from("author"),
            AppFrameParams::new(
                wire::app_tag(wire::MSG),
                Some(Nickname::from("bob")),
                None,
                body,
            ),
        )
        .with_id(id);
        assert!(
            !classify(&view(frame)).valid(),
            "a frame that is not a valid view is classified invalid"
        );
    }

    // No regression: legitimate addressing still classifies valid.
    #[test]
    fn legitimate_addressing_stays_valid() {
        assert!(
            classify(&status_frame(Some(Nickname::from("bob")))).valid(),
            "a directed status is valid"
        );
        assert!(
            classify(&chat_frame(None)).valid(),
            "a broadcast chat message is valid"
        );
        assert!(
            classify(&msg_frame(Some(Nickname::from("bob")))).valid(),
            "a directed chat message is valid"
        );
    }
}
