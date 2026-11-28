//! [`send_broadcast`] is the single source of truth for the chat send path —
//! the IPC `Msg` command and the typed in-process `SessionRequest` both funnel
//! through it so they cannot drift.

use std::collections::HashMap;
use std::time::Instant;

use habilis_network::embed::{EventLoopState, HandlerCtx};
use habilis_network::ops::blob::{ContentId, OffloadRequest};
use habilis_network::ops::{
    Delivered, MergeCtx, MeshSender, PendingSend, StateMergeParams, blob, broadcast_state_merge,
    seal_broadcast_body,
};
use habilis_network::protocol::messages::{Authored, Verified, WireFrame};
use habilis_network::protocol::{
    AppFrameParams, AuthorKey, Channel, Identity, MeshId, Message, MessageBody, MessageId,
    MessageKind, Nickname, Shard, ShardGroup, reassembly,
};
use habilis_network::util::consts::{MAX_LOGICAL_BODY_BYTES, MAX_MESSAGE_SIZE, MAX_SHARD_TOTAL};
use habilis_network::util::logging;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Sender;
use tokio::sync::oneshot::error::TryRecvError::{Closed, Empty};

use crate::a2a::a2a_app::{A2aApp, A2aResponder, A2aWaiter, DeliveredLeg, UnsentLeg};
use crate::a2a::extensions::META_SEQ;
use crate::a2a::rpc_error::{PeerRef, RpcError, is_send_message};
use crate::a2a::session_request::SessionRequest;
use crate::a2a::surfaced_event::PollResponder::Typed;
use crate::a2a::surfaced_event::{PollBatch, PollOrRegisterParams};
use crate::a2a::task_record::TaskRecord;
use crate::a2a::{
    META_LABEL, WorkerLeg, ipc_command, node, receipt, served, status_update_params, task_record,
    tuning, wire,
};
use crate::emission;

/// Write the dev log of a just-built outbound message. The operator echo is
/// the caller's responsibility — it differs by kind (`print_message_ex` for
/// `Msg`, `print_task` for `Task`).
fn log_outbound(message: &Verified) {
    logging::log_out(message);
}

/// Write the dev log of a just-built outbound `Msg` and — when `echo` — print
/// the operator line. The raw shards of a split body commit silently
/// (`echo == false`); only the reassembled logical message is echoed.
fn commit_outbound_part(
    state: &EventLoopState,
    message: &Verified,
    out: &emission::Output,
    echo: bool,
) {
    if echo {
        let authored = message.authored();
        out.print_message_ex(&authored, true, addressee_name(state, &authored).as_ref());
    }
    log_outbound(message);
}

/// Headroom subtracted from a shard's body budget so the first shard's measured
/// envelope still covers later shards, whose `seq` may have grown a digit or two.
const PART_BUDGET_MARGIN: usize = 16;

/// Upper bound on a client-supplied A2A-call `timeout_secs`. Clamped before it is
/// added to a `tokio::time::Instant` (an unbounded value would overflow the
/// platform `Instant` and panic the event loop). Generous — any real RPC round
/// trip answers well inside an hour. App policy, so it lives here rather than in
/// the engine's `util::consts`.
pub(crate) const A2A_CALL_MAX_TIMEOUT_SECS: u64 = 3600;

/// JSON-escaped byte length of one char inside a `"…"` string. `serde_json` keeps
/// non-ASCII as UTF-8; only the quote, backslash, and `\n`/`\t`/`\r` expand.
/// Used to split a body so each shard's *serialized* size fits the wire cap.
fn escaped_char_len(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\n' | '\t' | '\r' => 2,
        ch if (ch as u32) < 0x20 => 6, // other controls → \u00XX (rejected by MessageBody)
        ch => ch.len_utf8(),
    }
}

/// The escaped-body budget per shard: the wire cap minus the serialized envelope
/// of an empty-body shard (built with worst-case header digits), minus margin.
fn shard_body_budget(empty_part: &Message) -> usize {
    MAX_MESSAGE_SIZE.saturating_sub(empty_part.wire_len() + PART_BUDGET_MARGIN)
}

/// Split `body` into the fewest UTF-8-safe chunks whose JSON-escaped length each
/// fits `budget`. `None` if it would need more than [`MAX_SHARD_TOTAL`] shards
/// (the caller refuses the send) — or `budget` is zero.
fn split_body(body: &str, budget: usize) -> Option<Vec<&str>> {
    if budget == 0 {
        return None;
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < body.len() {
        let mut used = 0;
        let mut end = start;
        for (offset, ch) in body[start..].char_indices() {
            let cost = escaped_char_len(ch);
            if used + cost > budget {
                break;
            }
            used += cost;
            end = start + offset + ch.len_utf8();
        }
        if end == start {
            return None; // a single char exceeds the budget — never, for a sane budget
        }
        chunks.push(&body[start..end]);
        start = end;
        if chunks.len() > MAX_SHARD_TOTAL as usize {
            return None;
        }
    }
    Some(chunks)
}

/// The identity + payload of one outbound chat frame — the fields `build_msg`
/// shares with `synthesize_logical_msg`, grouped so the shard/signer args stay
/// their own params.
struct ChatFrameParams<'caller> {
    mesh: &'caller MeshId,
    author: &'caller Nickname,
    body: MessageBody,
    id: Option<MessageId>,
}

/// Build, shard-tag and sign one outbound chat frame (without
/// serializing — the caller measures or serializes). `frame.id` pins the frame
/// id: the payload's A2A `messageId` for a single-frame body (`None` keeps the
/// minted id — shards of a split body keep their own ids; the *group* carries
/// the A2A id there).
fn build_msg(frame: ChatFrameParams<'_>, shard: Option<Shard>, signer: &Identity) -> Verified {
    let ChatFrameParams {
        mesh,
        author,
        body,
        id,
    } = frame;
    let mut message = Message::new_app(
        mesh,
        author,
        AppFrameParams::new(wire::app_tag(wire::BROADCAST), None, None, body),
    );
    if let Some(id) = id {
        message = message.id(id);
    }
    message.shard(shard).sign(signer)
}

/// Wait for the sends that run on a task of their own. The first failure is the verdict.
async fn settle(pending: Vec<PendingSend>, deadline: tokio::time::Instant) -> anyhow::Result<()> {
    for handle in pending {
        tokio::time::timeout_at(deadline, handle)
            .await
            .map_err(|_| anyhow::anyhow!("a send gave no outcome by its deadline"))?
            .map_err(|_| anyhow::anyhow!("a send task ended before it gave its outcome"))??;
    }
    Ok(())
}

/// Whether a leg waits in `A2aApp::unsent_legs` until every shard has a verdict: only a content leg
/// that does not fit one frame does. A single frame has its verdict at once, and a beat is never
/// split, so neither takes a slot, and neither can be refused for a full list that it never joins.
fn needs_unsent_slot(wire_len: usize, content: bool) -> bool {
    content && !fits_one_frame(wire_len)
}

/// Whether a frame of `wire_len` bytes goes out whole, and is not split into shards.
fn fits_one_frame(wire_len: usize) -> bool {
    wire_len <= MAX_MESSAGE_SIZE
}

/// The bytes that an own task leg holds in the queue while its send waits: the plaintext body.
fn own_leg_bytes(body: &str) -> usize {
    body.len()
}

/// The bytes that an own content leg holds in the list while its send waits: the sealed frames,
/// and for a split leg a second copy in the shard cache. The size of a file that an artifact
/// offloads is not known before the offload, so only its text counts.
fn unsent_leg_bytes(body: &str) -> usize {
    body.len().saturating_mul(2)
}

/// The moment by which the sends of one layer must have an outcome. A layer takes it where it is
/// built, not where its task first runs, so a layer that wraps another one is built later and
/// never expires before it. Two layers built in the same instant can fire in the same tick, and
/// then the outer layer can answer with its own timeout instead of the verdict of the inner one:
/// both are a failure, so the caller and the log still agree.
fn send_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now()
        + std::time::Duration::from_secs(tuning::send_outcome_timeout_secs())
}

/// Give the verdict of the sends to `then`, off the event loop. With nothing pending the verdict
/// is known now, and `then` runs at once.
pub(crate) fn when_sent(
    pending: Vec<PendingSend>,
    then: impl FnOnce(anyhow::Result<()>) + Send + 'static,
) {
    if pending.is_empty() {
        then(Ok(()));
        return;
    }
    let deadline = send_deadline();
    tokio::spawn(async move { then(settle(pending, deadline).await) });
}

/// For a caller with no one to tell: a failure of a send on a task is only logged.
pub(crate) fn log_pending(pending: Vec<PendingSend>) {
    if pending.is_empty() {
        return;
    }
    let deadline = send_deadline();
    tokio::spawn(async move {
        if let Err(error) = settle(pending, deadline).await {
            tracing::warn!(target: "agent_gossip::a2a", %error, "directed send failed");
        }
    });
}

fn log_unless_done(delivered: Delivered) {
    if let Delivered::Pending(pending) = delivered {
        log_pending(vec![pending]);
    }
}

/// Answer a session request once its sends have an outcome. Returns whether a send started.
fn answer_when_sent(
    outcome: anyhow::Result<(Authored, Vec<PendingSend>)>,
    response: Sender<anyhow::Result<Authored>>,
) -> bool {
    match outcome {
        Ok((message, pending)) => {
            when_sent(pending, move |verdict| {
                let _ = response.send(verdict.map(|()| message));
            });
            true
        }
        Err(error) => {
            let _ = response.send(Err(error));
            false
        }
    }
}

/// One outbound wire frame plus its echo flag — the part
/// [`send_msg_part`] commits/delivers, grouped so the state/sender/out handles
/// stay their own leading params.
struct OutboundPart<'source> {
    message: &'source Verified,
    bytes: WireFrame,
    echo: bool,
}

/// Broadcast (or buffer, while unmeshed) one fully-built `Msg` and commit it to
/// the per-author chain + log. `echo` gates the operator print so the raw shards
/// of a split body commit silently. Errors if the unmeshed buffer is full.
async fn send_msg_part(
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
    part: OutboundPart<'_>,
) -> anyhow::Result<()> {
    let OutboundPart {
        message,
        bytes,
        echo,
    } = part;
    if state.is_meshed() {
        commit_outbound_part(state, message, out, echo);
        // Single send decision: a directed message goes point-to-point over
        // unicast, a broadcast over gossip (see `transport::deliver`). Inline,
        // not `deliver_in_background`: the CLI needs the delivery verdict to
        // report an unreachable peer, and each IPC send is one-shot.
        log_unless_done(habilis_network::ops::deliver(message, bytes, state, sender).await?);
    } else if state.pending_outbound_mut().push((message.clone(), bytes)) {
        // Buffered until the meshed edge flushes it through the send decision.
        commit_outbound_part(state, message, out, echo);
    } else {
        // Full buffer: the frame reaches no plane, so the reported drop matches
        // reality.
        tracing::warn!("pending outbound buffer full; outbound message dropped");
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; message dropped"
        ));
    }
    Ok(())
}

/// The identity + text of one outbound chat broadcast — grouped so
/// `send_broadcast` stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct BroadcastParams<'caller> {
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) text: MessageBody,
}

/// The body of one broadcast in both forms: the plaintext the operator sees and
/// the body the wire carries, which is sealed on a passworded mesh.
struct BroadcastBody {
    plain: MessageBody,
    wire: MessageBody,
    encrypted: bool,
}

impl BroadcastBody {
    /// On a passworded mesh, seal the chat body so a relay or a captured frame
    /// can't read it. Everything downstream (dedup, shard reassembly) operates
    /// on the sealed frame; only the local echo + the returned Message carry
    /// plaintext.
    fn seal(state: &EventLoopState, plain: MessageBody) -> anyhow::Result<Self> {
        let wire = seal_broadcast_body(state, &plain)?;
        Ok(Self {
            plain,
            wire,
            encrypted: state.broadcast_key().is_some(),
        })
    }
}

/// The frames of one broadcast: the single frame, the probe, and each shard.
struct BroadcastFrames<'caller> {
    mesh: &'caller MeshId,
    author: &'caller Nickname,
    signer: Identity,
}

impl<'caller> BroadcastFrames<'caller> {
    fn new(mesh: &'caller MeshId, author: &'caller Nickname, state: &EventLoopState) -> Self {
        Self {
            mesh,
            author,
            // A clone, not a borrow: the shard loop signs after it borrows the state mutably.
            signer: state.identity().clone(),
        }
    }

    fn frame(&self, body: MessageBody, id: Option<MessageId>) -> ChatFrameParams<'caller> {
        ChatFrameParams {
            mesh: self.mesh,
            author: self.author,
            body,
            id,
        }
    }

    /// The whole payload in one frame, its id the A2A messageId.
    fn single(&self, body: MessageBody, id: MessageId) -> Verified {
        build_msg(self.frame(body, Some(id)), None, &self.signer)
    }

    /// One shard: an ordinary frame with its own minted id.
    fn shard(&self, chunk: MessageBody, shard: Shard) -> Verified {
        build_msg(self.frame(chunk, None), Some(shard), &self.signer)
    }

    /// An empty-body frame sized against the *largest* shard's envelope, whose
    /// index and total have the most digits.
    fn probe(&self, group: &ShardGroup) -> Verified {
        build_msg(
            self.frame(MessageBody::try_new("").expect("empty body is valid"), None),
            Some(probe_shard(group)),
            &self.signer,
        )
    }
}

/// The shard whose envelope is the largest of a group.
fn probe_shard(group: &ShardGroup) -> Shard {
    Shard::new(group.clone(), MAX_SHARD_TOTAL).index(MAX_SHARD_TOTAL - 1)
}

/// The group of a split body and the count of its shards.
struct ShardSeries {
    group: ShardGroup,
    total: u32,
}

impl ShardSeries {
    fn new(group: ShardGroup, chunk_count: usize) -> Self {
        Self {
            group,
            total: u32::try_from(chunk_count).expect("chunk count is bounded by MAX_SHARD_TOTAL"),
        }
    }

    fn shard(&self, index: usize) -> Shard {
        Shard::new(self.group.clone(), self.total)
            .index(u32::try_from(index).expect("idx is bounded by MAX_SHARD_TOTAL"))
    }
}

/// Atomic admission while unmeshed: all shards or none, so a half-buffered body
/// (which could never reassemble) never reaches peers.
fn ensure_buffer_room(
    state: &mut EventLoopState,
    shard_count: usize,
    what: &str,
) -> anyhow::Result<()> {
    if !state.is_meshed() && state.pending_outbound_mut().remaining() < shard_count {
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; multipart {what} dropped"
        ));
    }
    Ok(())
}

/// The shard frames of one directed leg or RPC: same addressee and kind, each
/// signed on its own.
struct DirectedShardFrames<'caller> {
    mesh: &'caller MeshId,
    author: &'caller Nickname,
    kind: &'caller MessageKind,
    signer: &'caller Identity,
}

impl DirectedShardFrames<'_> {
    fn shard(&self, body: MessageBody, shard: Shard) -> Verified {
        Message::new_frame(self.mesh, self.author, self.kind.clone(), body)
            .shard(Some(shard))
            .sign(self.signer)
    }

    /// The body budget of a shard, sized from an empty-body frame in the
    /// largest shard's envelope.
    fn body_budget(&self, group: &ShardGroup) -> usize {
        let empty = MessageBody::try_new("").expect("empty body is valid");
        shard_body_budget(&self.shard(empty, probe_shard(group)))
    }
}

/// A split broadcast body, ready to send shard by shard.
struct BroadcastShards<'run> {
    frames: &'run BroadcastFrames<'run>,
    series: ShardSeries,
    chunks: Vec<&'run str>,
}

impl<'run> BroadcastShards<'run> {
    /// # Errors
    /// The body needs more than [`MAX_SHARD_TOTAL`] shards, or the unmeshed
    /// buffer cannot hold all of them.
    fn plan(
        frames: &'run BroadcastFrames<'run>,
        state: &mut EventLoopState,
        body: &'run BroadcastBody,
        group: ShardGroup,
    ) -> anyhow::Result<Self> {
        let budget = shard_body_budget(&frames.probe(&group));
        // Split the *wire* (sealed, when encrypted) body — shards carry chunks of
        // the ciphertext envelope that reassemble into it; the receiver decrypts the
        // reassembled body. The echoed/returned logical still carries plaintext.
        let chunks = split_body(body.wire.as_str(), budget).ok_or_else(|| {
            // Report the wire length that is actually being split: when encrypted
            // it is the ~1.4x-inflated sealed body, which is why a chat that fits
            // passwordless can exceed the shard cap here.
            let note = if body.encrypted {
                " (sealing inflates the body ~1.4x)"
            } else {
                ""
            };
            anyhow::anyhow!(
                "message too large: a {}-byte wire body needs more than {MAX_SHARD_TOTAL} shards{note}",
                body.wire.as_str().len()
            )
        })?;
        ensure_buffer_room(state, chunks.len(), "message")?;
        Ok(Self {
            frames,
            series: ShardSeries::new(group, chunks.len()),
            chunks,
        })
    }

    /// Send every shard silently. Return the first, which the logical frame
    /// copies its author and chain from, and the group.
    async fn send(
        self,
        state: &mut EventLoopState,
        sender: &MeshSender,
        out: &emission::Output,
    ) -> anyhow::Result<(Verified, ShardGroup)> {
        let mut cache_frames = Vec::new();
        let mut header = None;
        for (index, chunk) in self.chunks.iter().enumerate() {
            let chunk_body =
                MessageBody::try_new(chunk).expect("a substring of a valid body is valid");
            let message = self.frames.shard(chunk_body, self.series.shard(index));
            if index == 0 {
                header = Some(message.clone());
            }
            let bytes = message.serialize()?;
            cache_frames.push(bytes.clone());
            send_msg_part(
                state,
                sender,
                out,
                OutboundPart {
                    message: &message,
                    bytes,
                    echo: false,
                },
            )
            .await?;
        }
        if !cache_frames.is_empty() {
            state
                .shard_cache_mut()
                .insert(self.series.group.clone(), cache_frames);
        }
        Ok((
            header.expect("a split body has a first shard"),
            self.series.group,
        ))
    }
}

/// The one frame of a broadcast that fits the wire cap, with the body it carries.
struct SingleBroadcast<'source> {
    frame: Verified,
    body: &'source BroadcastBody,
}

/// Commit and send the one frame of a broadcast.
async fn send_single_broadcast(
    single: SingleBroadcast<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
) -> anyhow::Result<(MessageId, Authored)> {
    let SingleBroadcast { frame, body } = single;
    let bytes = frame.serialize()?;
    let id = frame.id.clone();
    if body.encrypted {
        // Commit + gossip the sealed frame silently, then echo/return a
        // plaintext-bodied view so the operator/agent sees the text.
        send_msg_part(
            state,
            sender,
            out,
            OutboundPart {
                message: &frame,
                bytes,
                echo: false,
            },
        )
        .await?;
        let plain = frame
            .open_with(|_| Some(body.plain.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!("a sealed frame opens to the body it was sealed from")
            })?;
        out.print_message_ex(&plain, true, addressee_name(state, &plain).as_ref());
        return Ok((id, plain));
    }
    send_msg_part(
        state,
        sender,
        out,
        OutboundPart {
            message: &frame,
            bytes,
            echo: true,
        },
    )
    .await?;
    Ok((id, frame.authored()))
}

/// Build, sign, log and gossip-broadcast one outbound chat message. The
/// single source of truth for the send path: the CLI socket's IPC `Msg`
/// command and the typed in-process `SessionRequest::Broadcast` both funnel
/// through here so they cannot drift. `text` is the operator/agent input; it
/// is wrapped here — and only here — into the A2A payload the wire carries
/// (see [`crate::a2a::status_update_params::compose_broadcast`]), so role/context/extension
/// stamping cannot drift between callers. A payload too large for one frame
/// is transparently split into `shard`-tagged messages the receiver
/// reassembles; the returned [`Message`] is the whole logical frame either
/// way, its id pinned to the payload's A2A `messageId`.
///
/// # Errors
/// Refuses a body past the [`MAX_LOGICAL_BODY_BYTES`] input ceiling, propagates
/// a [`Message::serialize`] failure and a gossip broadcast error, errors if the
/// unmeshed pending-outbound buffer is full, and refuses a body that would need
/// more than [`MAX_SHARD_TOTAL`] shards.
pub(crate) async fn send_broadcast(
    params: BroadcastParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
) -> anyhow::Result<(MessageId, Authored)> {
    let BroadcastParams { mesh, author, text } = params;
    // Gate the caller's *logical* body up front. Downstream only ever sees the
    // composed, sealed, shard-split wire body, so without this an oversized
    // input does not get refused — it gets split into ~18k frames and dies as an
    // opaque "pending outbound buffer full", or floods every peer. Same ceiling
    // and same blob-channel advice as the RPC path (`send_rpc_frame`), so the
    // caller-facing limit is one number regardless of which path a body takes.
    if text.as_str().len() > MAX_LOGICAL_BODY_BYTES {
        return Err(anyhow::anyhow!(
            "message body too large: {} bytes exceeds the input ceiling \
             ({MAX_LOGICAL_BODY_BYTES} bytes); use the blob channel for files",
            text.as_str().len()
        ));
    }
    let payload = status_update_params::compose_broadcast(mesh, text.as_str());
    let payload_id = MessageId::try_new(payload.message_id.as_str())
        .expect("an a2a message id is a valid frame id");
    let body = BroadcastBody::seal(state, status_update_params::payload_body(&payload)?)?;
    let frames = BroadcastFrames::new(mesh, author, state);
    let single = frames.single(body.wire.clone(), payload_id);
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        return send_single_broadcast(
            SingleBroadcast {
                frame: single,
                body: &body,
            },
            state,
            sender,
            out,
        )
        .await;
    }
    // Too big: split the payload across shard-tagged frames. Each shard is an
    // ordinary frame with its own minted id, cached for shard repair; the
    // *group* carries the A2A messageId, so the reassembled logical frame — the
    // only thing echoed and returned — is named by it.
    let shards = BroadcastShards::plan(
        &frames,
        state,
        &body,
        ShardGroup::parse_uuid(payload.message_id.as_str())
            .expect("an a2a message id is a valid shard group"),
    )?;
    let (header, group) = shards.send(state, sender, out).await?;
    let logical = reassembly::synthesize_from(&header, body.plain, &group);
    out.print_message_ex(&logical, true, addressee_name(state, &logical).as_ref());
    Ok((logical.id.clone(), logical))
}

/// One worker-pushed task frame's identity, target-kind, payload/echo bodies,
/// and cap-tracking flag, plus the in-flight task registry it's ingested
/// into — grouped so `broadcast_directed_frame` stays within the argument budget
/// alongside its state/sender/out handles.
struct DirectedFrameParams<'caller> {
    mesh: &'caller MeshId,
    author: &'caller Nickname,
    /// `None` for a leg that belongs to no task — a msg. Its
    /// presence is also what decides whether the coarse task machine is
    /// advanced for our own leg.
    task_id: Option<&'caller crate::a2a::TaskId>,
    kind: MessageKind,
    payload_body: MessageBody,
    echo_body: MessageBody,
    content: bool,
    echo_as: EchoAs,
    /// Pins the logical frame's id instead of keeping the minted one. Chat
    /// requires it: `chat_common` rejects a payload whose `messageId` does not
    /// equal its frame id, so a msg must carry the A2A id on the frame. Task
    /// legs pin nothing — their payloads carry the task id instead.
    id: Option<MessageId>,
    app: &'caller mut A2aApp,
}

/// How a directed leg's self-echo renders. A task leg drives the task widget
/// (`event: "task"`); a msg is an ordinary chat line, so it must
/// take the chat path or the sender would never see its own msg.
#[derive(Clone, Copy)]
enum EchoAs {
    Task,
    Chat,
}

/// Send one already-composed **directed** frame — a worker's
/// `TaskStatusUpdate`/`TaskArtifactUpdate`, or a msg — sharding a
/// large body, caching the shards of a **content** leg for repair, echoing the sender's
/// own line, and (for a task leg) advancing our own coarse task machine.
/// `content` is false for a liveness beat (never retained/reassembled).
/// Fire-and-forget push — the A2A streaming plane over gossip.
///
/// The body arrives already sealed to the addressee; `echo_body` is its
/// plaintext twin, because we cannot decrypt what we sealed to someone else.
///
/// # Errors
/// Propagates a [`Message::serialize`] failure, a gossip broadcast error, and
/// a full unmeshed pending-outbound buffer.
async fn broadcast_directed_frame(
    frame: DirectedFrameParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
) -> anyhow::Result<(MessageId, Authored, Vec<PendingSend>)> {
    let DirectedFrameParams {
        mesh,
        author,
        task_id,
        kind,
        payload_body,
        echo_body,
        content,
        echo_as,
        id: pinned_id,
        app,
    } = frame;
    if task_id.is_some() {
        app.admit_own_leg(own_leg_bytes(echo_body.as_str()))?;
    }
    let signer = state.identity().clone();
    // Fast path: the whole leg in one frame. The id is pinned before signing —
    // the signature covers it.
    let mut single = Message::new_frame(mesh, author, kind.clone(), payload_body.clone());
    if let Some(pinned_id) = pinned_id {
        single = single.id(pinned_id);
    }
    let single = single.sign(&signer);
    // The logical id: a sharded leg's group carries it, so its receipt names it too.
    if let MessageKind::App { to: Some(to), .. } = &kind {
        app.note_sent_ref(single.id.as_str(), to);
    }
    if needs_unsent_slot(single.wire_len(), content) {
        // The sealed frames and the shard cache hold the body twice, until every shard has a verdict.
        app.admit_unsent_leg(unsent_leg_bytes(payload_body.as_str()))?;
    }
    if fits_one_frame(single.wire_len()) {
        let bytes = single.serialize()?;
        let id = single.id.clone();
        // The wire frame's directed body is sealed to the addressee, which we
        // cannot decrypt; surface *and ingest* the self-echo from a plaintext
        // twin instead (same id, never signed or sent).
        let echo = single
            .open_with(|_| Some(echo_body))
            .expect("a frame opens to the twin it was signed for");
        let pending = send_directed_leg(
            state,
            sender,
            out,
            DirectedLegPart {
                message: &single,
                bytes,
                echo: Some(&echo),
                content,
                echo_as,
            },
        )
        .await?;
        let pending =
            ingest_own_leg_when_sent(app, OwnLeg { echo, task_id }, pending.into_iter().collect());
        return Ok((id, single.authored(), pending));
    }
    // Only content legs are ever large enough to split; the beat is a tiny
    // liveness widget and is never retained/reassembled.
    if !content {
        return Err(anyhow::anyhow!("task beat leg too large to send"));
    }
    // The shard group carries the logical id, so the reassembled view is
    // named identically on both ends.
    let group =
        ShardGroup::parse_uuid(single.id.as_str()).expect("a frame id is a valid shard group");
    let shard_frames = DirectedShardFrames {
        mesh,
        author,
        kind: &kind,
        signer: &signer,
    };
    let budget = shard_frames.body_budget(&group);
    let chunks = split_body(payload_body.as_str(), budget).ok_or_else(|| {
        anyhow::anyhow!(
            "task leg too large: a {}-byte body needs more than {MAX_SHARD_TOTAL} shards",
            payload_body.as_str().len()
        )
    })?;
    ensure_buffer_room(state, chunks.len(), "task leg")?;
    let series = ShardSeries::new(group, chunks.len());
    let mut cache_frames = Vec::new();
    let mut frames = Vec::new();
    let mut shard_pending = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let chunk_body = MessageBody::try_new(chunk).expect("a substring of a valid body is valid");
        let message = shard_frames.shard(chunk_body, series.shard(index));
        let bytes = message.serialize()?;
        cache_frames.push(bytes.clone());
        // The shards enter the log as one group below, not one by one.
        shard_pending.push(
            send_directed_leg(
                state,
                sender,
                out,
                DirectedLegPart {
                    message: &message,
                    bytes,
                    echo: None,
                    content: false,
                    echo_as,
                },
            )
            .await?,
        );
        frames.push(message);
    }
    let cache = (!cache_frames.is_empty()).then(|| (series.group.clone(), cache_frames));
    let logical = reassembly::synthesize_from(&frames[0], payload_body, &series.group);
    let echo = reassembly::synthesize_from(&frames[0], echo_body, &series.group);
    let pending = queue_sharded_leg(app, frames, cache, shard_pending);
    // Echo + ingest the logical leg once; the echo is the plaintext twin
    // (the logical body is sealed).
    let addressee = addressee_name(state, &echo);
    let own_echo = OwnEcho {
        message: echo.clone(),
        echo_as,
        addressee,
    };
    let pending = echo_when_sent(out, own_echo, pending);
    let pending = ingest_own_leg_when_sent(app, OwnLeg { echo, task_id }, pending);
    Ok((logical.id.clone(), logical, pending))
}

/// The key that a send by nickname goes to: the one live key of `peer`, else the one quiet key.
/// A shared name has no key, so the send is refused. The caller resolves it once and uses the
/// same key to seal the body and to address the frame.
pub(crate) fn peer_key(state: &EventLoopState, peer: &Nickname) -> anyhow::Result<AuthorKey> {
    state
        .addressee_key(peer)
        .map_err(|error| anyhow::anyhow!("cannot address '{peer}': {error}"))
}

/// The key that holds the task a call names, when the record of that task has `peer` as its
/// counterparty. A follow-up, a read and a cancel go to that key. The name can be shared with
/// another live key, or can now belong to a key that holds no task.
fn recorded_counterparty(
    tasks: &HashMap<crate::a2a::TaskId, TaskRecord>,
    params: &serde_json::Value,
    peer: &Nickname,
) -> Option<AuthorKey> {
    let named: Option<&str> = params["message"]["taskId"]
        .as_str()
        .or_else(|| params["id"].as_str());
    let task_id = named.and_then(crate::a2a::TaskId::from_uuid_str)?;
    tasks
        .get(&task_id)
        .filter(|rec| rec.peer == *peer)
        .map(|rec| rec.peer_key)
}

/// The key a call goes to, or why the call is refused. A call on a task that we hold goes to the
/// key of the task record. Any other call resolves the name: it needs one live key under it,
/// except that an open task with the resolved key lets the call through a brief roster flap.
fn call_target(
    state: &EventLoopState,
    tasks: &HashMap<crate::a2a::TaskId, TaskRecord>,
    params: &serde_json::Value,
    peer: &PeerRef,
) -> Result<AuthorKey, String> {
    let peer = match peer {
        PeerRef::Nickname(nickname) => nickname,
        // A key names exactly one member: no name to resolve and none to share.
        PeerRef::Key(key) => {
            let is_present = state.known_nickname_of(key).is_some();
            return if is_present || holds_open_task_with(tasks, key) {
                Ok(*key)
            } else {
                Err(format!("unknown peer '{key}'"))
            };
        }
    };
    let recorded = recorded_counterparty(tasks, params, peer);
    let resolved: Option<AuthorKey> = recorded.or_else(|| peer_key(state, peer).ok());
    let party_to_a_task = resolved.is_some_and(|key| holds_open_task_with(tasks, &key));
    let is_present = match recorded {
        Some(key) => state.known_nickname_of(&key).is_some(),
        None => state.is_peer(peer.as_str()),
    };
    if !party_to_a_task && !is_present {
        return Err(format!("unknown peer '{peer}'"));
    }
    let live_with_name = live_holders(state.live_nicknames(), peer);
    if recorded.is_none() && live_with_name > 1 {
        return Err(format!(
            "ambiguous peer '{peer}': {live_with_name} live members use this name"
        ));
    }
    resolved.ok_or_else(|| format!("ambiguous or unknown peer '{peer}'"))
}

/// Whether `tasks` holds a live task with the member `key`. The key is the identity: a name that
/// another key holds now is not the counterparty of a task that the first key opened.
fn holds_open_task_with(tasks: &HashMap<crate::a2a::TaskId, TaskRecord>, key: &AuthorKey) -> bool {
    tasks
        .values()
        .any(|rec| rec.peer_key == *key && !rec.state.is_terminal())
}

/// The addressee's name for a directed line, read from the roster at print time. `None` for a
/// broadcast, or when the addressee is not live.
pub(crate) fn addressee_name(state: &EventLoopState, message: &Authored) -> Option<Nickname> {
    let MessageKind::App { to: Some(key), .. } = &message.kind else {
        return None;
    };
    state.nickname_of(key)
}

/// Print a directed leg's self-echo down the surface its kind belongs to.
fn print_echo(
    out: &emission::Output,
    echo: &Authored,
    echo_as: EchoAs,
    addressee: Option<&Nickname>,
) {
    match echo_as {
        EchoAs::Task => out.print_task(
            echo,
            emission::TaskView {
                is_self: true,
                addressee_name: addressee,
                quarter_moved: false,
            },
        ),
        EchoAs::Chat => out.print_message_ex(echo, true, addressee),
    }
}

/// A worker-emitted status leg's identity, task, state/note, and the task
/// registry it's resolved (and later ingested) against — grouped so
/// `emit_task_status` stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct TaskStatusParams<'caller> {
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) task_id: &'caller crate::a2a::TaskId,
    pub(crate) leg: WorkerLeg,
    pub(crate) app: &'caller mut A2aApp,
}

/// Worker-emit a `TaskStatusUpdate` for a task we're serving: resolve the other
/// party from the task record and push the status to it.
///
/// A `working` leg is a check-in: it says what changed and how far the work is. The
/// [`WorkerLeg`] holds that rule, so every caller meets it before the request reaches the
/// daemon, and the receiving side drops a `working` leg that breaks it.
///
/// # Errors
/// `unknown task` if we hold no record for `task_id`; otherwise a serialize/broadcast
/// failure.
pub(crate) async fn emit_task_status(
    params: TaskStatusParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
) -> anyhow::Result<(Authored, Vec<PendingSend>)> {
    let TaskStatusParams {
        mesh,
        author,
        task_id,
        leg,
        app,
    } = params;
    let Some(peer_key) = app.tasks.get(task_id).map(|rec| rec.peer_key) else {
        return Err(anyhow::anyhow!("unknown task '{task_id}'"));
    };
    broadcast_task_status(
        BroadcastTaskStatusParams {
            mesh,
            author,
            peer_key: &peer_key,
            task_id,
            leg,
            app,
        },
        state,
        sender,
        out,
    )
    .await
    .map(|(_id, message, pending)| (message, pending))
}

/// A worker-emitted artifact leg's identity, task, result text/file, and the
/// task registry it's resolved (and later ingested) against — grouped so
/// `emit_task_artifact` stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct TaskArtifactEmitParams<'caller> {
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) task_id: &'caller crate::a2a::TaskId,
    pub(crate) text: &'caller str,
    pub(crate) file: Option<FileRef>,
    pub(crate) app: &'caller mut A2aApp,
}

/// Worker-emit a `TaskArtifactUpdate` (the result) for a task we're serving.
///
/// # Errors
/// An artifact with no text and no file, which would park the task for review
/// with nothing to review; `unknown task` if we hold no record for `task_id`;
/// otherwise a serialize/broadcast failure.
pub(crate) async fn emit_task_artifact(
    params: TaskArtifactEmitParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
) -> anyhow::Result<(Authored, Vec<PendingSend>)> {
    let TaskArtifactEmitParams {
        mesh,
        author,
        task_id,
        text,
        file,
        app,
    } = params;
    if text.trim().is_empty() && file.is_none() {
        anyhow::bail!("an artifact needs a text or a file");
    }
    let Some(peer_key) = app.tasks.get(task_id).map(|rec| rec.peer_key) else {
        return Err(anyhow::anyhow!("unknown task '{task_id}'"));
    };
    broadcast_task_artifact(
        BroadcastTaskArtifactParams {
            mesh,
            author,
            peer_key: &peer_key,
            task_id,
            text,
            file,
            app,
        },
        state,
        sender,
        out,
    )
    .await
    .map(|(_id, message, pending)| (message, pending))
}

/// Seal a directed payload `body` to the published X25519 key of the member `to`. A directed
/// frame is **never** sent in plaintext — if the seal key of the recipient has
/// not arrived yet, this errors rather than leaking the body. The Ed25519
/// frame signature (added later, over this ciphertext) authenticates the sender.
///
/// # Errors
/// The recipient's seal key is not yet known (its `PeerInfo` is not here yet), or the
/// seal operation fails.
pub(crate) fn seal_directed(
    state: &EventLoopState,
    to: &AuthorKey,
    body: &MessageBody,
) -> anyhow::Result<MessageBody> {
    let key = state.seal_key_of(to).ok_or_else(|| {
        let known: Option<Nickname> = state.known_nickname_of(to);
        let name: String = known.map_or_else(|| to.to_string(), |nickname| nickname.to_string());
        anyhow::anyhow!("cannot seal to '{name}': its encryption key is not known yet")
    })?;
    habilis_network::protocol::seal_to_body(key, body.as_str()).map_err(anyhow::Error::from)
}

/// A worker-emitted status leg's identity, addressee, task, state/note, and
/// the task registry the frame is ingested into — grouped so
/// `broadcast_task_status` stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct BroadcastTaskStatusParams<'caller> {
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) peer_key: &'caller AuthorKey,
    pub(crate) task_id: &'caller crate::a2a::TaskId,
    pub(crate) leg: WorkerLeg,
    pub(crate) app: &'caller mut A2aApp,
}

/// The status leg a worker emits: its state and note, the order value that
/// tells it apart from the other legs of its second, and the check-in fields.
fn worker_status_leg(leg: StatusLegParams<'_>) -> crate::a2a::TaskStatusUpdate {
    let StatusLegParams { mesh, task_id, leg } = leg;
    let metadata = status_update_params::status_metadata(
        leg.waiting_user(),
        leg.progress(),
        status_update_params::leg_order_ms(),
    );
    let update =
        status_update_params::StatusUpdateParams::new(task_id, leg.state()).with_metadata(metadata);
    let update = match leg.note() {
        Some(note) => update.with_note(note),
        None => update,
    };
    status_update_params::status_update(mesh, update)
}

/// The fields of one worker status leg, grouped so [`worker_status_leg`] stays
/// within the argument budget.
#[derive(Clone, Copy)]
struct StatusLegParams<'caller> {
    mesh: &'caller MeshId,
    task_id: &'caller crate::a2a::TaskId,
    leg: &'caller WorkerLeg,
}

/// A worker-emitted `TaskStatusUpdate` (`a2a status`): compose the A2A status
/// payload and push it to the key `peer_key` fire-and-forget.
///
/// # Errors
/// Propagates a serialize/broadcast failure.
pub(crate) async fn broadcast_task_status(
    params: BroadcastTaskStatusParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
) -> anyhow::Result<(MessageId, Authored, Vec<PendingSend>)> {
    let BroadcastTaskStatusParams {
        mesh,
        author,
        peer_key,
        task_id,
        leg,
        app,
    } = params;
    let update = worker_status_leg(StatusLegParams {
        mesh,
        task_id,
        leg: &leg,
    });
    let plain = status_update_params::payload_body(&update)?;
    let to = *peer_key;
    let sealed = seal_directed(state, &to, &plain)?;
    let kind = MessageKind::App {
        tag: wire::app_tag(wire::STATUS),
        to: Some(to),
        corr: None,
    };
    broadcast_directed_frame(
        DirectedFrameParams {
            mesh,
            author,
            task_id: Some(task_id),
            kind,
            payload_body: sealed,
            echo_body: plain,
            content: true,
            echo_as: EchoAs::Task,
            id: None,
            app,
        },
        state,
        sender,
        out,
    )
    .await
}

/// The identity, addressee and text of one outbound msg — grouped
/// so [`send_msg`] stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct MsgParams<'caller> {
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) peer: &'caller Nickname,
    pub(crate) text: MessageBody,
    pub(crate) app: &'caller mut A2aApp,
}

/// Send one msg: a chat line addressed to a single peer. The
/// counterpart of [`send_broadcast`], and the single source of truth for the
/// msg send path.
///
/// Three properties, each load-bearing and none of them incidental:
///
/// - **Directed**, so [`habilis_network::ops::deliver`] routes it unicast —
///   a `to: Some(..)` frame structurally cannot ride gossip, so no other peer
///   receives the bytes at all.
/// - **Sealed** to the addressee, so the multihop relays that *do* carry it
///   cannot read the body.
/// - **Unchained.** It never touches [`build_msg`]/`commit_outbound_part`, the
///   paths that stamp `seq`/`prev` and advance the DAG. A msg reaches only its
///   addressee, so a chain entry for it would be an unfillable gap in this
///   author's chain for every other peer — which fork detection reads as a
///   fork.
///
/// # Errors
/// Refuses a body past the [`MAX_LOGICAL_BODY_BYTES`] input ceiling, errors if
/// the addressee's seal key has not replicated yet (rather than sending in
/// plaintext), and propagates a serialize/deliver failure or a full unmeshed
/// pending-outbound buffer.
/// The live members that use `peer`'s name. Two live keys under one name make the name ambiguous.
pub(crate) fn live_holders<'a>(live: impl Iterator<Item = &'a Nickname>, peer: &Nickname) -> usize {
    live.filter(|nick| *nick == peer).count()
}

pub(crate) async fn send_msg(
    params: MsgParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
) -> anyhow::Result<(MessageId, Authored, Vec<PendingSend>)> {
    let MsgParams {
        mesh,
        author,
        peer,
        text,
        app,
    } = params;
    // Same ceiling as the broadcast and RPC paths, so the caller-facing limit is
    // one number regardless of which path a body takes.
    if text.as_str().len() > MAX_LOGICAL_BODY_BYTES {
        return Err(anyhow::anyhow!(
            "message body too large: {} bytes exceeds the input ceiling \
             ({MAX_LOGICAL_BODY_BYTES} bytes); use the blob channel for files",
            text.as_str().len()
        ));
    }
    let live_with_name = live_holders(state.live_nicknames(), peer);
    if live_with_name > 1 {
        return Err(anyhow::anyhow!(
            "ambiguous peer '{peer}': {live_with_name} live members use this name"
        ));
    }
    let payload = status_update_params::compose_msg(mesh, text.as_str());
    let payload_id = MessageId::try_new(payload.message_id.as_str())
        .expect("an a2a message id is a valid frame id");
    let plain = status_update_params::payload_body(&payload)?;
    let to = peer_key(state, peer)?;
    let sealed = seal_directed(state, &to, &plain)?;
    let kind = MessageKind::App {
        tag: wire::app_tag(wire::MSG),
        to: Some(to),
        corr: None,
    };
    broadcast_directed_frame(
        DirectedFrameParams {
            mesh,
            author,
            task_id: None,
            kind,
            payload_body: sealed,
            echo_body: plain,
            content: true,
            echo_as: EchoAs::Chat,
            id: Some(payload_id),
            app,
        },
        state,
        sender,
        out,
    )
    .await
}

/// A worker-emitted artifact leg's identity, addressee, task, result
/// text/file, and the task registry the frame is ingested into — grouped so
/// `broadcast_task_artifact` stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct BroadcastTaskArtifactParams<'caller> {
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) peer_key: &'caller AuthorKey,
    pub(crate) task_id: &'caller crate::a2a::TaskId,
    pub(crate) text: &'caller str,
    pub(crate) file: Option<FileRef>,
    pub(crate) app: &'caller mut A2aApp,
}

/// A worker-emitted `TaskArtifactUpdate` (`a2a artifact`): compose the A2A
/// artifact payload and push it to the key `peer_key` fire-and-forget.
///
/// # Errors
/// Propagates a serialize/broadcast failure.
pub(crate) async fn broadcast_task_artifact(
    params: BroadcastTaskArtifactParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
) -> anyhow::Result<(MessageId, Authored, Vec<PendingSend>)> {
    let BroadcastTaskArtifactParams {
        mesh,
        author,
        peer_key,
        task_id,
        text,
        file,
        app,
    } = params;
    // Before the offload: a refused artifact must not copy its file into the blob store for nothing.
    app.admit_own_leg(own_leg_bytes(text))?;
    let parts = build_offload_parts(
        OffloadTextParams {
            mesh,
            author,
            task_id,
            text,
            file,
        },
        state,
        app,
    )
    .await?;
    let update = status_update_params::artifact_update_parts(mesh, task_id, parts);
    let plain = status_update_params::payload_body(&update)?;
    let to = *peer_key;
    let sealed = seal_directed(state, &to, &plain)?;
    let kind = MessageKind::App {
        tag: wire::app_tag(wire::ARTIFACT),
        to: Some(to),
        corr: None,
    };
    broadcast_directed_frame(
        DirectedFrameParams {
            mesh,
            author,
            task_id: Some(task_id),
            kind,
            payload_body: sealed,
            echo_body: plain,
            content: true,
            echo_as: EchoAs::Task,
            id: None,
            app,
        },
        state,
        sender,
        out,
    )
    .await
}

/// The identity + task-target of a `--file`-bearing leg — grouped so
/// `build_offload_parts` stays within the argument budget alongside its
/// state/app handles.
struct OffloadTextParams<'caller> {
    mesh: &'caller MeshId,
    author: &'caller Nickname,
    task_id: &'caller crate::a2a::TaskId,
    text: &'caller str,
    file: Option<FileRef>,
}

/// A file to offload onto an A2A part: the path plus the optional
/// `filename`/`mediaType` to advertise (the `--file` / `--file-name` /
/// `--file-mime` surface). The engine's blob channel takes only the path — the
/// A2A part shape is this layer's business.
#[derive(Debug)]
pub(crate) struct FileRef {
    pub(crate) path: std::path::PathBuf,
    pub(crate) name: Option<String>,
    pub(crate) mime: Option<String>,
}

/// Resolve the parts of a `--file`-bearing leg: with no file, a single text
/// part (today's behavior); with a file, offload it over the blob channel into a
/// `Part.url` reference (+ the text part when non-empty). Lazily binds this
/// peer's blob server on the first offload. The heavy read+hash runs off the
/// event loop inside `blob::offload`.
async fn build_offload_parts(
    params: OffloadTextParams<'_>,
    state: &EventLoopState,
    app: &mut A2aApp,
) -> anyhow::Result<Vec<crate::a2a::Part>> {
    let OffloadTextParams {
        mesh,
        author,
        task_id,
        text,
        file,
    } = params;
    let Some(file) = file else {
        return Ok(vec![crate::a2a::Part::text(text)]);
    };
    let parsed: habilis_network::protocol::LockedMesh = mesh.as_str().parse().map_err(|error| {
        anyhow::anyhow!("cannot resolve mesh lookups for blob offload: {error}")
    })?;
    let lookups = parsed.config().lookups.clone();
    let relay_transport = parsed
        .config()
        .transport
        .allows(habilis_network::protocol::Transport::Relay);
    // Route through the choke point so the base is validated (0700, ours) before
    // this attachment payload spool is created — bypassing it could birth the
    // shared base at a world-traversable 0755.
    let spool = crate::ensure_mesh_runtime_dir(mesh.as_str())
        .map_err(|error| anyhow::anyhow!("cannot prepare blob spool dir: {error}"))?
        .join(format!("{author}.blobs"));
    // Every offloaded blob inherits the mesh password (if any), so a scraped
    // ticket can't be redeemed without it.
    let password = state.mesh_password();
    let ticket = blob::offload(
        &mut app.blob_server,
        &lookups,
        relay_transport.into(),
        OffloadRequest::new(file.path, ContentId::new(task_id.as_str()), spool)
            .password(password.cloned()),
    )
    .await?;
    // The engine hands back a ticket; turning it into a `Part.url` is A2A's shape.
    let part = crate::a2a::Part {
        url: Some(ticket.encode()),
        filename: file.name,
        media_type: file.mime,
        ..crate::a2a::Part::default()
    };
    let mut parts = vec![part];
    if !text.is_empty() {
        parts.push(crate::a2a::Part::text(text));
    }
    Ok(parts)
}

/// Who attaches which file to an `a2a call` — grouped so [`attach_call_file`]
/// stays within the argument budget alongside its state/app handles.
pub(crate) struct CallFile<'source> {
    pub(crate) mesh: &'source MeshId,
    pub(crate) author: &'source Nickname,
    pub(crate) file: FileRef,
}

/// Offload `file` and put its `Part.url` first among the parts of the
/// `SendMessage` in `params` (`a2a call --file`). An empty text part — the
/// `--text`-less sugar — is dropped, as `build_offload_parts` drops it.
///
/// The blob goes in the content group of the task that the message names, which the task sweep
/// evicts when it reaps the task.
///
/// # Errors
/// When `params` carries no A2A `message` with a `taskId`, or the offload fails.
pub(crate) async fn attach_call_file(
    params: &mut serde_json::Value,
    attachment: CallFile<'_>,
    state: &EventLoopState,
    app: &mut A2aApp,
) -> anyhow::Result<()> {
    let CallFile { mesh, author, file } = attachment;
    let slot = params
        .get_mut("message")
        .ok_or_else(|| anyhow::anyhow!("--file needs a SendMessage call with a message"))?;
    let mut message: crate::a2a::Message = serde_json::from_value(slot.take())?;
    let group_id = message
        .task_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("--file needs a SendMessage call with a task"))?;
    let mut parts = build_offload_parts(
        OffloadTextParams {
            mesh,
            author,
            task_id: &group_id,
            text: "",
            file: Some(file),
        },
        state,
        app,
    )
    .await?;
    parts.extend(
        message
            .parts
            .into_iter()
            .filter(|part| part.text.as_deref() != Some("")),
    );
    message.parts = parts;
    *slot = serde_json::to_value(message)?;
    Ok(())
}

/// One outbound directed leg's wire frame/bytes plus its echo twin and
/// cap-tracking flag — the part [`send_directed_leg`] retains/delivers, grouped
/// so the state/sender/out handles stay their own leading params.
struct DirectedLegPart<'source> {
    echo_as: EchoAs,
    message: &'source Verified,
    bytes: WireFrame,
    echo: Option<&'source Authored>,
    content: bool,
}

/// Broadcast (or buffer, while unmeshed) one fully-built task leg. `echo` carries the
/// plaintext twin whose operator line surfaces (`None` for the raw shards of a split
/// leg, which commit silently). Errors if the unmeshed buffer is full.
async fn send_directed_leg(
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
    part: DirectedLegPart<'_>,
) -> anyhow::Result<Option<PendingSend>> {
    let DirectedLegPart {
        message,
        bytes,
        echo,
        content,
        echo_as,
    } = part;
    if state.is_meshed() {
        // Meshed: a leg is logged, and its shards are cached for repair, only once
        // it was sent (see `A2aApp::unsent_legs`). Directed legs go unicast, broadcasts
        // gossip (see `transport::deliver`).
        return match habilis_network::ops::deliver(message, bytes, state, sender).await? {
            Delivered::Done => {
                retain_leg(message, content);
                if let Some(echo) = echo {
                    print_echo(out, echo, echo_as, addressee_name(state, echo).as_ref());
                }
                Ok(None)
            }
            Delivered::Pending(pending) => {
                retain_leg(message, content);
                let pending = match echo {
                    Some(echo) => {
                        let addressee = addressee_name(state, echo);
                        let own_echo = OwnEcho {
                            message: echo.clone(),
                            echo_as,
                            addressee,
                        };
                        echo_when_sent(out, own_echo, vec![pending])
                    }
                    None => vec![pending],
                };
                Ok(pending.into_iter().next())
            }
            _ => Err(anyhow::anyhow!("unsupported delivery outcome")),
        };
    } else if state.pending_outbound_mut().push((message.clone(), bytes)) {
        // Buffered until the meshed edge flushes it through the send decision.
        retain_leg(message, content);
        if let Some(echo) = echo {
            print_echo(out, echo, echo_as, addressee_name(state, echo).as_ref());
        }
    } else {
        tracing::warn!("pending outbound buffer full; outbound message dropped");
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; message dropped"
        ));
    }
    Ok(None)
}

/// Write the dev log of a task leg. Content legs log; the beat doesn't. The echo
/// is the caller's, and it waits for the delivery verdict.
fn retain_leg(message: &Verified, content: bool) {
    if content {
        log_outbound(message);
    }
}

/// Queue the shards of one leg for the dev log and the shard cache, as one group: they
/// enter only when every shard was sent. One shard in the cache lets `shard/repair`
/// rebuild a leg whose send failed. `pending` holds the handle of each shard,
/// `None` for a shard already sent. The handles are replaced by one that gives the verdict of
/// the whole leg.
fn queue_sharded_leg(
    app: &mut A2aApp,
    frames: Vec<Verified>,
    cache: Option<(ShardGroup, Vec<WireFrame>)>,
    pending: Vec<Option<PendingSend>>,
) -> Vec<PendingSend> {
    let pending: Vec<PendingSend> = pending.into_iter().flatten().collect();
    let (sent_tx, sent) = oneshot::channel();
    app.unsent_legs.push(UnsentLeg {
        frames,
        cache,
        sent,
    });
    if pending.is_empty() {
        let _ = sent_tx.send(true);
        return pending;
    }
    let (verdict_tx, verdict_rx) = oneshot::channel();
    let deadline = send_deadline();
    tokio::spawn(async move {
        let verdict = settle(pending, deadline).await;
        let _ = sent_tx.send(verdict.is_ok());
        let _ = verdict_tx.send(verdict);
    });
    vec![verdict_rx]
}

/// Take the own legs whose send has an outcome. The legs that failed are dropped.
fn take_sent_legs(app: &mut A2aApp) -> Vec<UnsentLeg> {
    let mut sent = Vec::new();
    app.unsent_legs.retain_mut(|leg| match leg.sent.try_recv() {
        Ok(true) => {
            sent.push(UnsentLeg {
                frames: std::mem::take(&mut leg.frames),
                cache: leg.cache.take(),
                sent: oneshot::channel().1,
            });
            false
        }
        Ok(false) | Err(Closed) => false,
        Err(Empty) => true,
    });
    sent
}

/// Move the own legs that were sent into the dev log and the shard cache, and forget the
/// legs whose send failed.
pub(crate) fn retain_sent_legs(app: &mut A2aApp, state: &mut EventLoopState) {
    for leg in take_sent_legs(app) {
        for frame in &leg.frames {
            log_outbound(frame);
        }
        if let Some((group, frames)) = leg.cache {
            state.shard_cache_mut().insert(group, frames);
        }
    }
}

/// An own line to print once its sends succeed: the plaintext twin of the frame, the surface it
/// goes down, and the addressee's name as the roster gave it when the line was sent.
struct OwnEcho {
    message: Authored,
    echo_as: EchoAs,
    addressee: Option<Nickname>,
}

/// Print `echo` once the sends in `pending` all succeed, and give the verdict on
/// to the caller. A failed send prints nothing: the sender must not read a
/// message that never arrived as sent. With nothing pending the verdict is known
/// now, and the echo prints at once.
fn echo_when_sent(
    out: &emission::Output,
    echo: OwnEcho,
    pending: Vec<PendingSend>,
) -> Vec<PendingSend> {
    let OwnEcho {
        message,
        echo_as,
        addressee,
    } = echo;
    if pending.is_empty() {
        print_echo(out, &message, echo_as, addressee.as_ref());
        return pending;
    }
    let out = out.clone();
    let (verdict_tx, verdict_rx) = oneshot::channel();
    let deadline = send_deadline();
    tokio::spawn(async move {
        let verdict = settle(pending, deadline).await;
        if verdict.is_ok() {
            print_echo(&out, &message, echo_as, addressee.as_ref());
        }
        let _ = verdict_tx.send(verdict);
    });
    vec![verdict_rx]
}

/// Advance our own coarse task state machine for a leg we just sent. `msg`
/// must be the **plaintext twin** of the leg, never the wire frame: the wire
/// body is sealed to the addressee, so a status leg's state could not be read
/// back out of it. The `task_id` is threaded in for the same reason it always
/// was — the artifact leg never needs its body at all.
pub(crate) fn ingest_own_leg(app: &mut A2aApp, message: &Authored, task_id: &crate::a2a::TaskId) {
    task_record::ingest(
        &mut app.tasks,
        task_record::IngestLegParams {
            frame: message,
            task_id,
            mine: true,
            now: Instant::now(),
        },
    );
}

/// One own task leg: its plaintext twin and its task. A chat msg has no task and no record.
struct OwnLeg<'source> {
    echo: Authored,
    task_id: Option<&'source crate::a2a::TaskId>,
}

/// Queue an own task leg for the task record, behind the legs sent before it. The record takes a
/// leg when the peer got it, and in send order: a leg to a cold peer that waits on its dial holds
/// back the later legs, and a leg whose dial failed leaves no record. A leg with nothing pending
/// is delivered already. The handles are replaced by one that gives the same verdict, after the
/// queue has it: the answer to the caller then comes after the record can take the leg.
fn ingest_own_leg_when_sent(
    app: &mut A2aApp,
    leg: OwnLeg<'_>,
    pending: Vec<PendingSend>,
) -> Vec<PendingSend> {
    let OwnLeg { echo, task_id } = leg;
    let Some(task_id) = task_id else {
        return pending;
    };
    let delivered = DeliveredLeg {
        echo,
        task_id: task_id.clone(),
    };
    if pending.is_empty() {
        let (queue_tx, queue_rx) = oneshot::channel();
        let _ = queue_tx.send(true);
        app.queue_own_leg(delivered, queue_rx);
        app.drain_delivered_legs();
        return pending;
    }
    let (queue_tx, queue_rx) = oneshot::channel();
    let (verdict_tx, verdict_rx) = oneshot::channel();
    app.queue_own_leg(delivered, queue_rx);
    let deadline = send_deadline();
    tokio::spawn(async move {
        let verdict = settle(pending, deadline).await;
        let _ = queue_tx.send(verdict.is_ok());
        let _ = verdict_tx.send(verdict);
    });
    vec![verdict_rx]
}

/// Client side of the gossip A2A request/response: mint an `rpc_id`, park a
/// waiter (keyed by `rpc_id` + `peer`, with `timeout`), and broadcast an
/// `A2aReq` directed at `peer` carrying the JSON-RPC `{method, params}`. The
/// peer serves the safe method set and replies with an `A2aResp` that the
/// receive path routes into the waiter (`app.fulfill_a2a_waiter`); the
/// Ask the authors of our stalled big (unlogged) partial groups to re-send the
/// missing shards — the repair half of big-group reliability (the author half is
/// `state.shard_cache_mut()`, served by `resend_cached_shards`). Runs on the app tick;
/// fire-and-forget: no waiter is parked, the repair *is* the retry (the next
/// tick asks again while the group survives its TTL). A group that can't be
/// sealed yet (peer card still propagating) or whose send fails just waits for
/// the next round.
pub(crate) async fn send_shard_repair_requests(
    mesh: &MeshId,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &MeshSender,
) {
    let tickets: Vec<_> = state
        .reassembly_mut()
        .repair_tickets(Instant::now())
        .collect();
    for ticket in tickets {
        let envelope = serde_json::json!({
            "method": "shard/repair",
            "params": { "group": ticket.group.as_str(), "missing": ticket.missing },
        });
        let Ok(body) = MessageBody::try_new(&envelope.to_string()) else {
            continue;
        };
        let author_key = ticket.author;
        let sealed = match seal_directed(state, &author_key, &body) {
            Ok(sealed) => sealed,
            Err(error) => {
                tracing::debug!(%error, author = %ticket.author, "cannot seal shard repair request yet");
                continue;
            }
        };
        let corr = wire::corr_id(crate::a2a::A2aRpcId::random().as_str());
        let frame = Message::new_app(
            mesh,
            author,
            AppFrameParams::new(
                wire::app_tag(wire::REQ),
                Some(author_key),
                Some(corr),
                sealed,
            ),
        )
        .sign(state.identity());
        logging::log_out(&frame);
        match frame.serialize() {
            // In the background: this runs from `on_tick`, inline on the event
            // loop, where a cold `deliver` would stop the node for the dial.
            Ok(bytes) => {
                if !habilis_network::ops::deliver_in_background(&frame, bytes, state, sender).await
                {
                    tracing::debug!("shard repair request not sent; next tick retries");
                }
            }
            Err(error) => tracing::debug!(%error, "shard repair request serialize failed"),
        }
    }
}

/// A directed gossip A2A call's identity, target, JSON-RPC method/params/
/// timeout, and the transport-specific responder it answers through —
/// grouped so `broadcast_a2a_call` stays within the argument budget alongside
/// its state/app/sender handles.
pub(crate) struct BroadcastA2aCallParams<'caller> {
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) peer: PeerRef,
    pub(crate) method: &'caller str,
    pub(crate) params: serde_json::Value,
    pub(crate) timeout: std::time::Duration,
    pub(crate) responder: A2aResponder,
    /// `a2a call --file`: attached only once the peer check passes, so a call
    /// that cannot go out spools nothing.
    pub(crate) file: Option<FileRef>,
}

/// The peer a call goes to, as the record of a task it opens names it.
struct CallPeer {
    key: AuthorKey,
    name: Option<Nickname>,
}

/// Stamp the `SendMessage` in `params` as the initiator. An opening gets a task id that this node
/// mints, `mesh:seq` 1, and a record in `submitted` that exists from now on, so a call that times
/// out leaves a task to follow. A follow-up gets the next `mesh:seq` of the record it names.
/// Returns the id of the task that the call opened.
///
/// # Errors
/// An opening over the quota or to a peer with no name, and a follow-up for a task that this node
/// does not hold as its initiator.
fn stamp_send_message(
    params: &mut serde_json::Value,
    peer: &CallPeer,
    app: &mut A2aApp,
) -> Result<Option<crate::a2a::TaskId>, RpcError> {
    let Some(message) = params.get_mut("message").filter(|slot| slot.is_object()) else {
        return Ok(None);
    };
    let (task_id, seq, opened) = if message["taskId"].is_null() {
        if let Some(error) = node::task_admission_error(app, &peer.key) {
            return Err(error);
        }
        let name = peer
            .name
            .clone()
            .ok_or_else(|| RpcError::invalid_params("the peer of a new task has no name"))?;
        let task_id = crate::a2a::TaskId::random();
        let mut rec = TaskRecord::open(
            (peer.key, name),
            task_record::TaskRole::Initiator,
            crate::a2a::TaskState::Submitted,
            Instant::now(),
        );
        rec.seq = 1;
        rec.label = message["metadata"][META_LABEL]
            .as_str()
            .and_then(status_update_params::clean_label);
        app.tasks.insert(task_id.clone(), rec);
        (task_id.clone(), 1, Some(task_id))
    } else {
        let named = message["taskId"]
            .as_str()
            .and_then(crate::a2a::TaskId::from_uuid_str);
        let held = named.as_ref().and_then(|task_id| {
            app.tasks.get_mut(task_id).filter(|rec| {
                rec.role == task_record::TaskRole::Initiator && rec.peer_key == peer.key
            })
        });
        let Some(rec) = held else {
            return Err(RpcError::invalid_params(
                "SendMessage names a taskId with no task on this node",
            ));
        };
        rec.seq += 1;
        (
            named.unwrap_or_else(crate::a2a::TaskId::random),
            rec.seq,
            None,
        )
    };
    message["taskId"] = serde_json::Value::from(task_id.as_str());
    if !message["metadata"].is_object() {
        message["metadata"] = serde_json::json!({});
    }
    message["metadata"][META_SEQ] = serde_json::Value::from(seq);
    Ok(opened)
}

/// What [`prepare_call`] needs to know about the call beyond its params and handles.
struct PreparedCall<'caller> {
    mesh: &'caller MeshId,
    author: &'caller Nickname,
    method: &'caller str,
    peer: &'caller PeerRef,
    peer_key: AuthorKey,
    file: Option<FileRef>,
}

/// Stamp a `SendMessage` as the initiator and attach its file, so that the call is ready to seal.
/// Returns the task that the call opened.
///
/// # Errors
/// What [`stamp_send_message`] refuses, and a file that cannot be attached. The record of an
/// opening is taken back in that case.
async fn prepare_call(
    call: PreparedCall<'_>,
    params: &mut serde_json::Value,
    state: &EventLoopState,
    app: &mut A2aApp,
) -> Result<Option<crate::a2a::TaskId>, RpcError> {
    let PreparedCall {
        mesh,
        author,
        method,
        peer,
        peer_key,
        file,
    } = call;
    let opening = if is_send_message(method) {
        let name = match peer {
            PeerRef::Nickname(nickname) => Some(nickname.clone()),
            PeerRef::Key(key) => state.known_nickname_of(key),
        };
        let call_peer = CallPeer {
            key: peer_key,
            name,
        };
        stamp_send_message(params, &call_peer, app)?
    } else {
        None
    };
    let Some(file) = file else {
        return Ok(opening);
    };
    let attachment = CallFile { mesh, author, file };
    match attach_call_file(params, attachment, state, app).await {
        Ok(()) => Ok(opening),
        Err(error) => {
            app.drop_opening(opening.as_ref());
            Err(RpcError::invalid_params(&error.to_string()))
        }
    }
}

/// The waiter times out via the loop's a2a-deadline arm. Fails fast (through the
/// responder) when `peer` isn't a current peer or the waiter registry
/// is full — no silent park.
pub(crate) async fn broadcast_a2a_call(
    call: BroadcastA2aCallParams<'_>,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    sender: &MeshSender,
) {
    let BroadcastA2aCallParams {
        mesh,
        author,
        peer,
        method,
        params,
        timeout,
        responder,
        file,
    } = call;
    let mut params = params;
    let rpc_error = |code: i64, message: &str| {
        serde_json::json!({ "error": { "code": code, "message": message } }).to_string()
    };
    // Fast-fail an unknown peer, EXCEPT when we already hold a **live** task
    // with this peer: a follow-up / read / cancel into a live task must
    // survive a brief roster flap (the waiter's timeout is the feedback if
    // the peer is truly gone), so it isn't wedged. A terminal record — the
    // peer left, timed out, or the task completed — no longer justifies
    // parking a new call for its full deadline. Task creation (we hold no
    // task with the peer yet) and any read/cancel to a peer we share no task
    // with still require the peer present — method-agnostic, so
    // `SendStreamingMessage` creates gate the same as `SendMessage`.
    let peer_key = match call_target(state, &app.tasks, &params, &peer) {
        Ok(key) => key,
        Err(refusal) => {
            responder.send_response(&rpc_error(-32602, &refusal));
            return;
        }
    };
    let prepared = prepare_call(
        PreparedCall {
            mesh,
            author,
            method,
            peer: &peer,
            peer_key,
            file,
        },
        &mut params,
        state,
        app,
    )
    .await;
    let opening = match prepared {
        Ok(opening) => opening,
        Err(error) => {
            responder.send_response(&rpc_error(error.code, &error.message));
            return;
        }
    };
    if let Some(message_id) = params["message"]["messageId"].as_str() {
        app.note_sent_ref(message_id, &peer_key);
    }
    let envelope = serde_json::json!({ "method": method, "params": params });
    let Ok(body) = MessageBody::try_new(&envelope.to_string()) else {
        app.drop_opening(opening.as_ref());
        responder.send_response(&rpc_error(
            -32602,
            "request params contain control characters",
        ));
        return;
    };
    // Seal the request to the addressee — a relay forwards it but cannot read it.
    let body = match seal_directed(state, &peer_key, &body) {
        Ok(sealed) => sealed,
        Err(error) => {
            app.drop_opening(opening.as_ref());
            responder.send_response(&rpc_error(-32603, &error.to_string()));
            return;
        }
    };
    // The a2a layer mints a UUID rpc id; the engine correlates on the generic
    // `CorrId` carried by the app frame.
    let corr = wire::corr_id(crate::a2a::A2aRpcId::random().as_str());
    let sent_corr = corr.clone();
    let kind = MessageKind::App {
        tag: wire::app_tag(wire::REQ),
        to: Some(peer_key),
        corr: Some(corr.clone()),
    };
    let deadline = call_deadline(timeout);
    if let Some(unregistered) = app.register_a2a_waiter(A2aWaiter {
        corr,
        peer_key,
        deadline,
        responder,
        opening: opening.clone(),
        mesh: mesh.clone(),
    }) {
        app.drop_opening(opening.as_ref());
        unregistered.send_response(&rpc_error(-32603, "too many in-flight a2a calls"));
        return;
    }
    // Directed request over the shared RPC sender: unicast per frame,
    // transparently splitting a large sealed body into shard frames.
    let sent = send_directed_rpc(
        DirectedRpcParams {
            mesh,
            author,
            kind,
            body,
        },
        state,
        sender,
    )
    .await;
    let dispatched = DispatchedCall {
        method,
        peer_key,
        corr: &sent_corr,
        opening: opening.as_ref(),
    };
    settle_dispatch(sent, &dispatched, &params, app);
}

/// The deadline of a call: `timeout` from now, clamped before it is added to the `Instant`, since
/// an unclamped client `timeout_secs` would overflow the platform `Instant` and panic the loop.
fn call_deadline(timeout: std::time::Duration) -> tokio::time::Instant {
    let max_timeout = std::time::Duration::from_secs(A2A_CALL_MAX_TIMEOUT_SECS);
    if timeout > max_timeout {
        tracing::warn!(
            requested_secs = timeout.as_secs(),
            capped_secs = A2A_CALL_MAX_TIMEOUT_SECS,
            "a2a call timeout clamped to the maximum"
        );
    }
    tokio::time::Instant::now() + timeout.min(max_timeout)
}

/// The call whose request was just sent, as [`settle_dispatch`] needs it.
#[derive(Clone, Copy)]
struct DispatchedCall<'caller> {
    method: &'caller str,
    peer_key: AuthorKey,
    corr: &'caller habilis_network::protocol::CorrId,
    opening: Option<&'caller crate::a2a::TaskId>,
}

/// Book the outcome of sending a call's request. A follow-up hands the task back to the worker at
/// dispatch, so a reply that never arrives leaves no record parked on us with the check-in clock
/// stopped. An opening that never left is refused now: parked, it would report a task that the
/// worker never heard of.
fn settle_dispatch(
    sent: anyhow::Result<()>,
    call: &DispatchedCall<'_>,
    params: &serde_json::Value,
    app: &mut A2aApp,
) {
    let DispatchedCall {
        method,
        peer_key,
        corr,
        opening,
    } = *call;
    match sent {
        Ok(()) => {
            if is_send_message(method)
                && opening.is_none()
                && let Some(task_id) = params["message"]["taskId"]
                    .as_str()
                    .and_then(crate::a2a::TaskId::from_uuid_str)
            {
                task_record::note_follow_up_sent(
                    &mut app.tasks,
                    &task_id,
                    &peer_key,
                    Instant::now(),
                );
            }
        }
        Err(error) if opening.is_some() => {
            tracing::warn!(target: "agent_gossip::a2a", %error, "a2a opening send failed");
            if let Some(waiter) = app.take_a2a_waiter(corr) {
                let refusal = serde_json::json!({
                    "error": { "code": -32603, "message": error.to_string() }
                });
                waiter.responder.send_response(&refusal.to_string());
            }
            app.drop_opening(opening);
        }
        Err(error) if method == served::READER_GET => {
            tracing::debug!(target: "agent_gossip::a2a", %error, "reader query send failed");
        }
        Err(error) => {
            tracing::warn!(target: "agent_gossip::a2a", %error, "a2a request send failed");
        }
    }
}

/// A directed RPC frame's identity, kind, and body — the fields
/// `send_directed_rpc` needs beyond its state/sender handles.
pub(crate) struct DirectedRpcParams<'caller> {
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) kind: MessageKind,
    pub(crate) body: MessageBody,
}

/// Send one directed RPC frame (an `App` request/response), transparently
/// splitting a sealed body too large for one frame into shard-tagged frames —
/// each an ordinary directed frame riding unicast, so the RPC plane is
/// size-transparent like chat and task legs. RPC
/// frames are plumbing (never cached for repair); the shards reassemble
/// only in the receiver's dedicated store, and a big group is served from the
/// sender-side `ShardCache` (an engine internal) through the `shard/repair`
/// RPC.
///
/// # Errors
/// Propagates a serialize/deliver failure and refuses a body past the input
/// ceiling or the receiver's per-group reassembly budget.
pub(crate) async fn send_directed_rpc(
    params: DirectedRpcParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
) -> anyhow::Result<()> {
    let DirectedRpcParams {
        mesh,
        author,
        kind,
        body,
    } = params;
    let signer = state.identity().clone();
    let single = Message::new_frame(mesh, author, kind.clone(), body.clone()).sign(&signer);
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        logging::log_out(&single);
        let bytes = single.serialize()?;
        log_unless_done(habilis_network::ops::deliver(&single, bytes, state, sender).await?);
        return Ok(());
    }
    // The RPC body is already sealed — gate on the sealed length of the largest
    // logical body, so the caller-facing limit stays `MAX_LOGICAL_BODY_BYTES`.
    if body.as_str().len() > habilis_network::ops::sealed_body_len_bound(MAX_LOGICAL_BODY_BYTES) {
        return Err(anyhow::anyhow!(
            "rpc body too large: {} sealed bytes exceeds the input ceiling \
             ({MAX_LOGICAL_BODY_BYTES} raw bytes); use the blob channel for files",
            body.as_str().len()
        ));
    }
    let group = ShardGroup::parse_uuid(crate::a2a::A2aRpcId::random().as_str())
        .expect("a freshly minted uuid is a valid shard group");
    let shard_frames = DirectedShardFrames {
        mesh,
        author,
        kind: &kind,
        signer: &signer,
    };
    let chunks = split_body(body.as_str(), shard_frames.body_budget(&group)).ok_or_else(|| {
        anyhow::anyhow!("rpc body too large: needs more than {MAX_SHARD_TOTAL} shards")
    })?;
    ensure_buffer_room(state, chunks.len(), "rpc")?;
    let series = ShardSeries::new(group, chunks.len());
    let mut cache_frames = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let chunk_body = MessageBody::try_new(chunk).expect("a substring of a valid body is valid");
        let message = shard_frames.shard(chunk_body, series.shard(index));
        logging::log_out(&message);
        let bytes = message.serialize()?;
        cache_frames.push(bytes.clone());
        log_unless_done(habilis_network::ops::deliver(&message, bytes, state, sender).await?);
    }
    if !cache_frames.is_empty() {
        state.shard_cache_mut().insert(series.group, cache_frames);
    }
    Ok(())
}

/// Start a fresh ping round carrying the responder; the deadline-driven
/// `finalize_ping_round` delivers the RTT rows through it. Mirrors the IPC
/// `Ping` handler, which passes no responder and emits the `ping_report`
/// event instead.
async fn session_ping(
    response: Sender<Vec<(Nickname, u64)>>,
    state: &mut EventLoopState,
    context: &HandlerCtx<'_>,
) -> bool {
    habilis_network::ops::start_ping_round(state, context, Some(response)).await;
    true
}

/// The identity and handles a session request is served through.
#[derive(Clone, Copy)]
struct SessionScope<'caller> {
    mesh: &'caller MeshId,
    author: &'caller Nickname,
    sender: &'caller MeshSender,
    output: &'caller emission::Output,
}

/// Broadcast one chat body and give the outcome to the requester. Returns
/// whether the broadcast went out.
async fn session_broadcast(
    body: MessageBody,
    response: Sender<anyhow::Result<Authored>>,
    scope: SessionScope<'_>,
    state: &mut EventLoopState,
) -> bool {
    let params = BroadcastParams {
        mesh: scope.mesh,
        author: scope.author,
        text: body,
    };
    let outcome = send_broadcast(params, state, scope.sender, scope.output)
        .await
        .map(|(_id, message)| message);
    let sent_ok = outcome.is_ok();
    let _ = response.send(outcome);
    sent_ok
}

/// The fields of a [`SessionRequest::Msg`].
struct SessionMsg(Nickname, MessageBody, Sender<anyhow::Result<Authored>>);

async fn session_msg(
    request: SessionMsg,
    scope: SessionScope<'_>,
    app: &mut A2aApp,
    state: &mut EventLoopState,
) -> bool {
    let SessionMsg(to, body, response) = request;
    let outcome = send_msg(
        MsgParams {
            mesh: scope.mesh,
            author: scope.author,
            peer: &to,
            text: body,
            app,
        },
        state,
        scope.sender,
        scope.output,
    )
    .await;
    answer_when_sent(
        outcome.map(|(_id, message, pending)| (message, pending)),
        response,
    )
}

/// The fields of a [`SessionRequest::TaskStatus`].
struct SessionTaskStatus(
    crate::a2a::TaskId,
    WorkerLeg,
    Sender<anyhow::Result<Authored>>,
);

async fn session_task_status(
    request: SessionTaskStatus,
    scope: SessionScope<'_>,
    app: &mut A2aApp,
    state: &mut EventLoopState,
) -> bool {
    let SessionTaskStatus(task_id, leg, response) = request;
    let outcome = emit_task_status(
        TaskStatusParams {
            mesh: scope.mesh,
            author: scope.author,
            task_id: &task_id,
            leg,
            app,
        },
        state,
        scope.sender,
        scope.output,
    )
    .await;
    answer_when_sent(outcome, response)
}

/// The fields of a [`SessionRequest::TaskArtifact`].
struct SessionTaskArtifact(
    crate::a2a::TaskId,
    String,
    Option<FileRef>,
    Sender<anyhow::Result<Authored>>,
);

async fn session_task_artifact(
    request: SessionTaskArtifact,
    scope: SessionScope<'_>,
    app: &mut A2aApp,
    state: &mut EventLoopState,
) -> bool {
    let SessionTaskArtifact(task_id, text, file, response) = request;
    let outcome = emit_task_artifact(
        TaskArtifactEmitParams {
            mesh: scope.mesh,
            author: scope.author,
            task_id: &task_id,
            text: &text,
            file,
            app,
        },
        state,
        scope.sender,
        scope.output,
    )
    .await;
    answer_when_sent(outcome, response)
}

/// The fields of a [`SessionRequest::Poll`].
struct SessionPoll(Option<u64>, bool, Sender<PollBatch>);

/// Same policy as the CLI/IPC `Poll` arm: respond now if events are buffered,
/// else (with `long`) park a typed waiter the loop fulfills/expires. A parked
/// waiter broadcasts nothing, so this never reports a send.
async fn session_poll(
    request: SessionPoll,
    context: &HandlerCtx<'_>,
    app: &mut A2aApp,
    state: &mut EventLoopState,
) {
    let SessionPoll(after, long, response) = request;
    if !long {
        receipt::send_read_receipts(app, state, context, after).await;
    }
    app.surfaced.poll_or_register(PollOrRegisterParams {
        after,
        long,
        now: tokio::time::Instant::now(),
        responder: Typed(response),
    });
}

/// The fields of a [`SessionRequest::A2aCall`].
struct SessionA2aCall(
    Nickname,
    String,
    serde_json::Value,
    std::time::Duration,
    Sender<serde_json::Value>,
);

async fn session_a2a_call(
    request: SessionA2aCall,
    scope: SessionScope<'_>,
    app: &mut A2aApp,
    state: &mut EventLoopState,
) {
    let SessionA2aCall(peer, method, params, timeout, response) = request;
    broadcast_a2a_call(
        BroadcastA2aCallParams {
            mesh: scope.mesh,
            author: scope.author,
            peer: PeerRef::Nickname(peer),
            method: &method,
            params,
            timeout,
            responder: A2aResponder::Typed(response),
            file: None,
        },
        state,
        app,
        scope.sender,
    )
    .await;
}

/// A merge into one channel document and the requester's answer.
struct SessionMerge(Channel, serde_json::Value, Sender<anyhow::Result<()>>);

/// Merge into a channel document and broadcast it. Returns whether the merge
/// went out.
async fn session_merge(
    request: SessionMerge,
    scope: SessionScope<'_>,
    state: &mut EventLoopState,
) -> bool {
    let SessionMerge(channel, merge, response) = request;
    let merge_ctx = MergeCtx::new(scope.mesh, scope.author, scope.sender, scope.output);
    let outcome =
        broadcast_state_merge(state, StateMergeParams::new(merge_ctx, channel, merge)).await;
    let sent = outcome.is_ok();
    let _ = response.send(outcome.map_err(anyhow::Error::from));
    sent
}

/// One typed in-process request plus the identity + app state it's served
/// against — grouped so `handle_session_request` stays within the argument
/// budget alongside its state/sender/output handles.
pub(crate) struct SessionRequestParams<'caller> {
    pub(crate) request: SessionRequest,
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) app: &'caller mut A2aApp,
}

/// Raw injection (adversarial only): the bytes go on the wire exactly as given. Nothing parses
/// them and nothing re-signs them, so the receiver's own checks decide what happens to them. A
/// failure to broadcast is the caller's error.
#[cfg(feature = "adversarial")]
async fn inject_raw_frame(bytes: bytes::Bytes, sender: &MeshSender) -> anyhow::Result<()> {
    sender
        .broadcast(WireFrame::fixture_raw(bytes))
        .await
        .map_err(|err| anyhow::anyhow!("injected frame did not broadcast: {err}"))
}

/// Handle one typed in-process [`SessionRequest`] (in-process). `Send`
/// broadcasts via the shared helper and echoes the canonical [`Message`]
/// back on the oneshot; `Poll` returns the join-horizon-filtered buffer.
/// Returns `true` if anything was broadcast so the caller can refresh
/// `last_sent_at` (mirrors `handle_ipc_command`).
pub(crate) async fn handle_session_request(
    params: SessionRequestParams<'_>,
    state: &mut EventLoopState,
    context: &HandlerCtx<'_>,
    output: &emission::Output,
) -> bool {
    let SessionRequestParams {
        request,
        mesh,
        author,
        app,
    } = params;
    let scope = SessionScope {
        mesh,
        author,
        sender: context.sender,
        output,
    };
    app.drain_delivered_legs();
    retain_sent_legs(app, state);
    match request {
        SessionRequest::Broadcast { body, response } => {
            session_broadcast(body, response, scope, state).await
        }
        SessionRequest::Msg { to, body, response } => {
            session_msg(SessionMsg(to, body, response), scope, app, state).await
        }
        SessionRequest::Poll {
            after,
            long,
            response,
        } => {
            session_poll(SessionPoll(after, long, response), context, app, state).await;
            false
        }
        SessionRequest::TaskStatus {
            task_id,
            leg,
            response,
        } => {
            let fields = SessionTaskStatus(task_id, leg, response);
            session_task_status(fields, scope, app, state).await
        }
        SessionRequest::TaskArtifact {
            task_id,
            text,
            file,
            response,
        } => {
            let fields = SessionTaskArtifact(task_id, text, file, response);
            session_task_artifact(fields, scope, app, state).await
        }
        SessionRequest::Peers { response } => {
            let _ = response.send(state.roster_snapshot());
            false
        }
        SessionRequest::StateMerge { merge, response } => {
            session_merge(SessionMerge(Channel::State, merge, response), scope, state).await
        }
        SessionRequest::StateGet { response } => {
            let _ = response.send(state.view(Channel::State));
            false
        }
        SessionRequest::MetaMerge { merge, response } => {
            session_merge(SessionMerge(Channel::Meta, merge, response), scope, state).await
        }
        SessionRequest::OwnKey { response } => {
            let _ = response.send(state.identity().author_key());
            false
        }
        SessionRequest::MetaGet { response } => {
            let _ = response.send(ipc_command::meta_view(state));
            false
        }
        SessionRequest::Ping { response } => session_ping(response, state, context).await,
        SessionRequest::A2aCall {
            peer,
            method,
            params: rpc_params,
            timeout,
            response,
        } => {
            let fields = SessionA2aCall(peer, method, rpc_params, timeout, response);
            session_a2a_call(fields, scope, app, state).await;
            true
        }
        // Raw injection (adversarial only): broadcast the bytes verbatim, no
        // signing or chain stamping — a malicious/crafted message on the wire.
        #[cfg(feature = "adversarial")]
        SessionRequest::InjectRaw { bytes, response } => {
            // The bytes go on the wire as given; see `inject_raw_frame`.
            let outcome = inject_raw_frame(bytes, scope.sender).await;
            let _ = response.send(outcome);
            false
        }
        // Simulated stream end (adversarial only): indistinguishable from
        // the real `None` arm as far as the event loop is concerned.
        #[cfg(feature = "adversarial")]
        SessionRequest::SeverGossip => {
            state.sever_gossip();
            tracing::warn!("gossip stream severed (adversarial); heal arm will resubscribe");
            false
        }
        #[cfg(feature = "adversarial")]
        SessionRequest::ShardsResent { response } => {
            let _ = response.send(app.shards_resent);
            false
        }
        #[cfg(feature = "adversarial")]
        SessionRequest::SealKeyOf { key, response } => {
            let _ = response.send(state.seal_key_of(&key).copied());
            false
        }
        // Reassembly accounting snapshot (adversarial only) for the
        // shard-budget tripwires.
        #[cfg(feature = "adversarial")]
        SessionRequest::ReassemblyStats { response } => {
            let _ = response.send(state.reassembly_mut().stats());
            false
        }
    }
}

#[cfg(test)]
mod split_body_tests {
    use habilis_network::util::consts::MAX_SHARD_TOTAL;

    use super::{escaped_char_len, split_body};

    #[test]
    fn escaped_len_counts_json_escapes() {
        assert_eq!(
            escaped_char_len('a'),
            1,
            "a plain ascii char counts one byte"
        );
        assert_eq!(
            escaped_char_len('"'),
            2,
            "a quote counts as two escape bytes"
        );
        assert_eq!(
            escaped_char_len('\\'),
            2,
            "a backslash counts as two escape bytes"
        );
        assert_eq!(
            escaped_char_len('\n'),
            2,
            "a newline counts as two escape bytes"
        );
        assert_eq!(
            escaped_char_len('世'),
            3,
            "kept as 3-byte UTF-8, not \\u-escaped"
        );
    }

    #[test]
    fn chunks_concatenate_back_and_respect_budget() {
        let body = "0123456789".repeat(6); // 60 ASCII bytes
        let chunks = split_body(&body, 16).expect("60 bytes at budget 16 fits in MAX shards");
        assert!(chunks.len() > 1, "the body must actually split");
        assert!(
            chunks.len() <= MAX_SHARD_TOTAL as usize,
            "the shard count stays within the protocol maximum"
        );
        assert_eq!(chunks.concat(), body, "chunks reassemble to the original");
        for chunk in &chunks {
            let escaped: usize = chunk.chars().map(escaped_char_len).sum();
            assert!(escaped <= 16, "each chunk's escaped length fits the budget");
        }
    }

    #[test]
    fn splits_multibyte_on_char_boundaries() {
        let body = "héllo🌍".repeat(8); // 2-byte é and 4-byte emoji
        let chunks = split_body(&body, 8).expect("fits in MAX shards");
        assert_eq!(chunks.concat(), body, "no char is split mid-codepoint");
        assert!(
            chunks.iter().all(|chunk| !chunk.is_empty()),
            "no shard is empty"
        );
    }

    #[test]
    fn refuses_a_body_needing_too_many_parts() {
        // One byte per shard would need one shard per byte; a body one byte past
        // `MAX_SHARD_TOTAL` therefore needs more than the cap and is refused.
        let body = "x".repeat(MAX_SHARD_TOTAL as usize + 1);
        assert!(
            split_body(&body, 1).is_none(),
            "a body of one shard is not split"
        );
    }
}

#[cfg(test)]
mod open_task_gate_tests {
    use std::collections::HashMap;
    use std::time::Instant;

    use habilis_network::protocol::{AuthorKey, Nickname};

    use crate::a2a::task_record::{TaskRecord, TaskRole};
    use crate::a2a::{TaskId, TaskState};

    fn working_task_of(peer_key: AuthorKey) -> TaskRecord {
        TaskRecord::open(
            (peer_key, Nickname::from("bob")),
            TaskRole::Initiator,
            TaskState::Working,
            Instant::now(),
        )
    }

    /// A call by the name of a gone task peer must reach the unknown-peer check. The task belongs
    /// to the key that opened it, and a key that holds the name now is another member.
    #[test]
    fn a_call_by_the_name_of_a_gone_task_peer_is_refused_as_unknown() {
        let mut tasks = HashMap::new();
        let gone = AuthorKey::from([0xb0; 32]);
        let holder_now = AuthorKey::from([0xb1; 32]);
        tasks.insert(TaskId::random(), working_task_of(gone));

        assert!(
            super::holds_open_task_with(&tasks, &gone),
            "the key that opened the task is its party"
        );
        assert!(
            !super::holds_open_task_with(&tasks, &holder_now),
            "a key that only shares the name is not a party to the task"
        );
    }

    /// The key of the record, not the key that the name resolves to now, is the counterparty of
    /// the task. A name that another key holds, or that two keys share, does not move the task.
    #[test]
    fn a_call_on_a_task_goes_to_the_key_of_its_record() {
        let mut tasks = HashMap::new();
        let task_id = TaskId::random();
        let holder = AuthorKey::from([0xb0; 32]);
        tasks.insert(task_id.clone(), working_task_of(holder));
        let follow_up = serde_json::json!({ "message": { "taskId": task_id.as_str() } });
        let read = serde_json::json!({ "id": task_id.as_str() });

        let by_message = super::recorded_counterparty(&tasks, &follow_up, &Nickname::from("bob"));
        let by_id = super::recorded_counterparty(&tasks, &read, &Nickname::from("bob"));
        let other_name = super::recorded_counterparty(&tasks, &read, &Nickname::from("carol"));

        assert_eq!(by_message, Some(holder), "a follow-up goes to the holder");
        assert_eq!(by_id, Some(holder), "a read goes to the holder");
        assert_eq!(
            other_name, None,
            "a call that names another peer is not redirected"
        );
    }
}

#[cfg(test)]
mod stamp_tests {
    use habilis_network::protocol::{AuthorKey, Nickname};

    use super::{CallPeer, stamp_send_message};
    use crate::a2a::a2a_app::A2aApp;
    use crate::a2a::task_record::TaskRole;
    use crate::a2a::tuning::TASKS_PER_PEER_CAP;
    use crate::a2a::{META_LABEL, TaskId, TaskState};

    fn bob() -> CallPeer {
        CallPeer {
            key: AuthorKey::from([0xb0; 32]),
            name: Some(Nickname::from("bob")),
        }
    }

    fn send(message: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "message": message })
    }

    /// The daemon of the initiator mints the id and opens the record before the request leaves.
    #[test]
    fn an_opening_gets_an_id_a_seq_of_one_and_a_submitted_record() {
        let mut app = A2aApp::new();
        let mut params = send(&serde_json::json!({
            "messageId": "m1",
            "metadata": { META_LABEL: "docs" },
        }));

        let opened = stamp_send_message(&mut params, &bob(), &mut app)
            .expect("an opening is admitted")
            .expect("an opening reports the task it made");

        assert_eq!(
            params["message"]["taskId"],
            opened.as_str(),
            "the minted id rides the message"
        );
        assert_eq!(
            params["message"]["metadata"]["mesh:seq"], 1,
            "the first message of a task carries seq 1"
        );
        let rec = &app.tasks[&opened];
        assert_eq!(
            rec.state,
            TaskState::Submitted,
            "the record starts submitted"
        );
        assert_eq!(rec.role, TaskRole::Initiator, "this node opened it");
        assert_eq!(
            rec.label.as_deref(),
            Some("docs"),
            "the record keeps the label of the brief"
        );
        assert_eq!(rec.seq, 1, "the record counts the opening");
    }

    /// Each follow-up takes the next seq, and the record holds the last one stamped.
    #[test]
    fn each_follow_up_takes_the_next_seq() {
        let mut app = A2aApp::new();
        let mut opening = send(&serde_json::json!({ "messageId": "m1" }));
        let task_id = stamp_send_message(&mut opening, &bob(), &mut app)
            .expect("an opening is admitted")
            .expect("an opening reports the task it made");

        for expected in [2, 3] {
            let mut follow_up = send(&serde_json::json!({
                "messageId": format!("m{expected}"),
                "taskId": task_id.as_str(),
            }));
            let opened = stamp_send_message(&mut follow_up, &bob(), &mut app)
                .expect("a follow-up on a held task is admitted");
            assert!(opened.is_none(), "a follow-up opens no task");
            assert_eq!(
                follow_up["message"]["metadata"]["mesh:seq"], expected,
                "the seq counts up"
            );
        }
        assert_eq!(
            app.tasks[&task_id].seq, 3,
            "the record holds the last seq stamped"
        );
    }

    /// A raw call that names an id this node holds no record for is refused here.
    #[test]
    fn a_follow_up_for_a_task_we_do_not_hold_is_refused() {
        let mut app = A2aApp::new();
        let mut params = send(&serde_json::json!({
            "messageId": "m1",
            "taskId": TaskId::random().as_str(),
        }));

        let error = stamp_send_message(&mut params, &bob(), &mut app)
            .expect_err("no record means no follow-up");

        assert_eq!(error.code, -32602, "the refusal is an invalid-params error");
        assert!(
            app.tasks.is_empty(),
            "a refused follow-up creates no record"
        );
    }

    /// A follow-up on a task of another peer is not ours to send to this one.
    #[test]
    fn a_follow_up_to_another_peer_than_the_workers_is_refused() {
        let mut app = A2aApp::new();
        let mut opening = send(&serde_json::json!({ "messageId": "m1" }));
        let task_id = stamp_send_message(&mut opening, &bob(), &mut app)
            .expect("an opening is admitted")
            .expect("an opening reports the task it made");
        let carol = CallPeer {
            key: AuthorKey::from([0xc0; 32]),
            name: Some(Nickname::from("carol")),
        };
        let mut follow_up = send(&serde_json::json!({
            "messageId": "m2",
            "taskId": task_id.as_str(),
        }));

        let error = stamp_send_message(&mut follow_up, &carol, &mut app)
            .expect_err("the task belongs to bob");

        assert_eq!(error.code, -32602, "the refusal is an invalid-params error");
        assert_eq!(
            app.tasks[&task_id].seq, 1,
            "the refused follow-up took no seq"
        );
    }

    /// The per-peer quota binds the initiator too: an opening over it is refused at dispatch.
    #[test]
    fn an_opening_over_the_peer_quota_is_refused() {
        let mut app = A2aApp::new();
        for _ in 0..TASKS_PER_PEER_CAP {
            let mut params = send(&serde_json::json!({ "messageId": "m" }));
            stamp_send_message(&mut params, &bob(), &mut app)
                .expect("an opening under the quota is admitted");
        }
        let mut over = send(&serde_json::json!({ "messageId": "over" }));

        let error =
            stamp_send_message(&mut over, &bob(), &mut app).expect_err("the quota is spent");

        assert_eq!(error.code, -32004, "the refusal is the quota code");
        assert_eq!(
            app.tasks.len(),
            TASKS_PER_PEER_CAP,
            "the refused opening made no record"
        );
        assert!(
            over["message"]["taskId"].is_null(),
            "the refused message is not stamped"
        );
    }
}
