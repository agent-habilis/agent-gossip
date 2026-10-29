//! [`send_broadcast`] is the single source of truth for the chat send path —
//! the IPC `Msg` command and the typed in-process `SessionRequest` both funnel
//! through it so they cannot drift.

use std::collections::HashMap;
use std::time::Instant;

use habilis_network::embed::{EventLoopState, HandlerCtx};
use habilis_network::ops::blob::{ContentId, OffloadRequest};
use habilis_network::ops::{
    Delivered, MergeCtx, MeshSender, PendingSend, StateMergeParams, blob, broadcast_state_merge,
    doc,
};
use habilis_network::protocol::message::{Authored, Verified, WireFrame};
use habilis_network::protocol::{
    AppFrameParams, Channel, ContentHash, Identity, MeshId, Message, MessageBody, MessageId,
    MessageKind, Nickname, Shard, ShardGroup, reassembly,
};
use habilis_network::util::consts::{
    LOGGED_SHARD_GROUP_MAX_TOTAL, MAX_LOGICAL_BODY_BYTES, MAX_MESSAGE_SIZE, MAX_SEALED_BODY_BYTES,
    MAX_SHARD_TOTAL,
};
use habilis_network::util::logging;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Sender;
use tokio::sync::oneshot::error::TryRecvError::{Closed, Empty};

use crate::a2a::a2a_app::{A2aApp, A2aResponder, A2aWaiter, DeliveredLeg, UnsentLeg};
use crate::a2a::session_request::SessionRequest;
use crate::a2a::surfaced_event::PollResponder::Typed;
use crate::a2a::surfaced_event::{PollBatch, PollOrRegisterParams};
use crate::a2a::task_record::TaskRecord;
use crate::a2a::{card, receipt, served, status_update_params, task_record, wire};
use crate::emission;

/// Retain a just-built outbound message in the local log (pruning the
/// fork/DAG indexes on any eviction) and write the dev log. The operator
/// echo is the caller's responsibility — it differs by kind
/// (`print_message_ex` for `Msg`, `print_task` for `Task`).
fn retain_outbound(state: &mut EventLoopState, message: &Verified) {
    if let Some(evicted) = state.message_log_mut().push(message.clone()) {
        let evicted_hash = evicted.content_hash();
        state.forget_hash(&evicted_hash);
        if let Some(seq) = evicted.seq {
            state.forget_msg_seq(&evicted.key(), seq, &evicted_hash);
        }
    }
    logging::log_out(message);
}

/// Commit a just-built outbound `Msg` into local state: advance the per-author
/// hash chain (`seq`/`prev`) and the DAG tips, retain it in the message log
/// (pruning the fork/DAG indexes on eviction), write the dev log, and — when
/// `echo` — print the operator line. The raw shards of a split body commit
/// silently (`echo == false`); only the reassembled logical message is echoed.
fn commit_outbound_part(
    state: &mut EventLoopState,
    message: &Verified,
    out: &emission::Output,
    echo: bool,
) {
    let hash = message.content_hash();
    state.advance_chain(hash);
    state.note_dag(hash, &message.parents, message.timestamp);
    if echo {
        out.print_message_ex(&message.authored(), true);
    }
    retain_outbound(state, message);
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

/// One outbound frame's position in the per-author hash chain + DAG: the
/// `seq`, the `prev` hash link, and the current DAG tips. Bundled so
/// [`build_msg`] stays within the argument budget.
struct ChainStamp {
    seq: u64,
    previous: Option<ContentHash>,
    parents: Vec<ContentHash>,
}

/// The identity + payload of one outbound chat frame — the fields `build_msg`
/// shares with [`synthesize_logical_msg`], grouped so the chain-stamp/shard/
/// signer args stay their own params.
struct ChatFrameParams<'caller> {
    mesh: &'caller MeshId,
    author: &'caller Nickname,
    body: MessageBody,
    id: Option<MessageId>,
}

/// Build, chain-stamp, shard-tag and sign one outbound chat frame (without
/// serializing — the caller measures or serializes). `frame.id` pins the frame
/// id: the payload's A2A `messageId` for a single-frame body (`None` keeps the
/// minted id — shards of a split body keep their own ids; the *group* carries
/// the A2A id there).
fn build_msg(
    frame: ChatFrameParams<'_>,
    stamp: ChainStamp,
    shard: Option<Shard>,
    signer: &Identity,
) -> Verified {
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
        message = message.with_id(id);
    }
    message
        .with_chain(stamp.seq, stamp.previous)
        .with_parents(stamp.parents)
        .with_shard(shard)
        .sign(signer)
}

/// Wait for the sends that run on a task of their own. The first failure is the verdict.
async fn settle(pending: Vec<PendingSend>) -> anyhow::Result<()> {
    for handle in pending {
        handle
            .await
            .map_err(|_| anyhow::anyhow!("a send task ended before it gave its outcome"))??;
    }
    Ok(())
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
    tokio::spawn(async move { then(settle(pending).await) });
}

/// For a caller with no one to tell: a failure of a send on a task is only logged.
pub(crate) fn log_pending(pending: Vec<PendingSend>) {
    if pending.is_empty() {
        return;
    }
    tokio::spawn(async move {
        if let Err(error) = settle(pending).await {
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
    /// can't read it. Everything downstream (dedup, the per-author chain, the
    /// message log, anti-entropy) operates on the sealed frame; only the local
    /// echo + the returned Message carry plaintext.
    fn seal(state: &EventLoopState, plain: MessageBody) -> anyhow::Result<Self> {
        let key = state.broadcast_key();
        let wire = match key {
            Some(key) => doc::encrypt_body(&plain, key)?,
            None => plain.clone(),
        };
        Ok(Self {
            plain,
            wire,
            encrypted: key.is_some(),
        })
    }
}

/// The frames of one broadcast. The chain position is read once, because every
/// frame this call emits — the single frame, the probe, and each shard — stamps
/// the *same* position, and only the commit of a frame advances it afterwards.
struct BroadcastFrames<'caller> {
    mesh: &'caller MeshId,
    author: &'caller Nickname,
    signer: Identity,
    chain_seq: u64,
    chain_prev: Option<ContentHash>,
}

impl<'caller> BroadcastFrames<'caller> {
    fn new(mesh: &'caller MeshId, author: &'caller Nickname, state: &EventLoopState) -> Self {
        let (chain_seq, chain_prev) = state.chain_head();
        Self {
            mesh,
            author,
            // A clone, not a borrow: the shard loop signs after it borrows the state mutably.
            signer: state.identity().clone(),
            chain_seq,
            chain_prev,
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

    fn stamp(&self, state: &EventLoopState) -> ChainStamp {
        ChainStamp {
            seq: self.chain_seq,
            previous: self.chain_prev,
            parents: state.dag_parents().collect(),
        }
    }

    /// The whole payload in one frame, its id the A2A messageId.
    fn single(&self, state: &EventLoopState, body: MessageBody, id: MessageId) -> Verified {
        build_msg(
            self.frame(body, Some(id)),
            self.stamp(state),
            None,
            &self.signer,
        )
    }

    /// One shard: an ordinary chained frame with its own minted id.
    fn shard(&self, state: &EventLoopState, chunk: MessageBody, shard: Shard) -> Verified {
        build_msg(
            self.frame(chunk, None),
            self.stamp(state),
            Some(shard),
            &self.signer,
        )
    }

    /// An empty-body frame sized against the *largest* shard's envelope, not the
    /// first's. Each later shard chains off the prior, so even from genesis
    /// (empty `prev`/`parents`) every shard after the first carries a 64-char
    /// `prev` and one parent hash. Worst-case both: a full-length `prev` and as
    /// many parent hashes as the first shard could hold (its current tips, never
    /// fewer than the one-hash link later shards carry). The all-zero hash
    /// stands in for any content hash.
    fn probe(&self, state: &EventLoopState, group: &ShardGroup) -> Verified {
        let hash_stub = ContentHash::from([0; 32]);
        build_msg(
            self.frame(MessageBody::try_new("").expect("empty body is valid"), None),
            ChainStamp {
                seq: self.chain_seq,
                previous: Some(hash_stub),
                parents: vec![hash_stub; state.dag_parents().count().max(1)],
            },
            Some(probe_shard(group)),
            &self.signer,
        )
    }
}

/// The shard whose envelope is the largest of a group.
fn probe_shard(group: &ShardGroup) -> Shard {
    Shard::new(group.clone(), MAX_SHARD_TOTAL).with_index(MAX_SHARD_TOTAL - 1)
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
            .with_index(u32::try_from(index).expect("idx is bounded by MAX_SHARD_TOTAL"))
    }

    /// A big group's shards skip the message log, so peers can only heal a lost
    /// shard via the `shard/repair` RPC, served from the sender-side frame
    /// cache. Small groups heal through anti-entropy instead.
    fn is_cached(&self) -> bool {
        self.total > LOGGED_SHARD_GROUP_MAX_TOTAL
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
            .with_shard(Some(shard))
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
        let budget = shard_body_budget(&frames.probe(state, &group));
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
            let message = self
                .frames
                .shard(state, chunk_body, self.series.shard(index));
            if index == 0 {
                header = Some(message.clone());
            }
            let bytes = message.serialize()?;
            if self.series.is_cached() {
                cache_frames.push(bytes.clone());
            }
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
        out.print_message_ex(&plain, true);
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
    let single = frames.single(state, body.wire.clone(), payload_id);
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
    // ordinary chained frame with its own minted id, retained for
    // anti-entropy; the *group* carries the A2A messageId, so the reassembled
    // logical frame — the only thing echoed and returned — is named by it.
    let shards = BroadcastShards::plan(
        &frames,
        state,
        &body,
        ShardGroup::from_uuid_str(payload.message_id.as_str())
            .expect("an a2a message id is a valid shard group"),
    )?;
    let (header, group) = shards.send(state, sender, out).await?;
    let logical = reassembly::synthesize_from(&header, body.plain, &group);
    out.print_message_ex(&logical, true);
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
/// large body, retaining **content** legs for anti-entropy, echoing the sender's
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
    let signer = state.identity().clone();
    // Fast path: the whole leg in one frame. The id is pinned before signing —
    // the signature covers it.
    let mut single = Message::new_frame(mesh, author, kind.clone(), payload_body.clone());
    if let Some(pinned_id) = pinned_id {
        single = single.with_id(pinned_id);
    }
    let single = single.sign(&signer);
    // The logical id: a sharded leg's group carries it, so its receipt names it too.
    if let MessageKind::App { to: Some(to), .. } = &kind {
        app.note_sent_ref(single.id.as_str(), to);
    }
    if single.wire_len() <= MAX_MESSAGE_SIZE {
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
                app,
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
        ShardGroup::from_uuid_str(single.id.as_str()).expect("a frame id is a valid shard group");
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
        if series.is_cached() {
            cache_frames.push(bytes.clone());
        }
        // The shards enter the log as one group below, not one by one.
        shard_pending.push(
            send_directed_leg(
                state,
                sender,
                out,
                DirectedLegPart {
                    app,
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
    let pending = echo_when_sent(out, echo.clone(), echo_as, pending);
    let pending = ingest_own_leg_when_sent(app, OwnLeg { echo, task_id }, pending);
    Ok((logical.id.clone(), logical, pending))
}

/// Print a directed leg's self-echo down the surface its kind belongs to.
fn print_echo(out: &emission::Output, echo: &Authored, echo_as: EchoAs) {
    match echo_as {
        EchoAs::Task => out.print_task(echo, true),
        EchoAs::Chat => out.print_message_ex(echo, true),
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
    pub(crate) task_state: crate::a2a::TaskState,
    pub(crate) note: Option<&'caller str>,
    /// Why a `working` task waits on the worker's own user; rides the leg as
    /// `mesh:waiting`.
    pub(crate) waiting: Option<&'caller str>,
    pub(crate) app: &'caller mut A2aApp,
}

/// Worker-emit a `TaskStatusUpdate` for a task we're serving: resolve the other
/// party from the task record and push the status to it. `--text` becomes the
/// status note and nothing more — there is no progress reporting here. A
/// `done/total` fraction can only ride a *beat*, and the sole beat producer is
/// the daemon keepalive, which sources its fraction from `last_fraction` — a
/// field written only from a *received* beat. The loop has no source, so the
/// fraction is unreachable on every node. Giving a worker real progress needs a
/// producer, and it is not a one-liner: `beat_metadata` sets the beat marker, and
/// `ingest` routes any beat-marked status to `LegKind::Beat`, which does not
/// advance state — so naively tagging a `working` status with a fraction would
/// swallow the transition.
///
/// # Errors
/// `waiting requires state working` if a reason comes with another state;
/// `unknown task` if we hold no record for `task_id`; otherwise a
/// serialize/broadcast failure.
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
        task_state,
        note,
        waiting,
        app,
    } = params;
    let leg = WorkerLeg::parse(task_state, waiting)?;
    let Some(peer) = app.tasks.get(task_id).map(|rec| rec.peer.clone()) else {
        return Err(anyhow::anyhow!("unknown task '{task_id}'"));
    };
    broadcast_task_status(
        BroadcastTaskStatusParams {
            mesh,
            author,
            peer: &peer,
            task_id,
            leg,
            note,
            app,
        },
        state,
        sender,
        out,
    )
    .await
    .map(|(_id, message, pending)| (message, pending))
}

/// A worker's status leg, parsed once from its state and waiting reason. The
/// reason rides a `working` leg only, so no other state can hold one. Only
/// [`WorkerLeg::parse`] builds a `WorkerLeg`, so the rule has one place.
#[derive(Clone, Copy)]
pub(crate) enum WorkerLeg<'source> {
    Working { waiting: Option<&'source str> },
    Other(crate::a2a::TaskState),
}

impl<'a> WorkerLeg<'a> {
    pub(crate) fn parse(
        state: crate::a2a::TaskState,
        waiting: Option<&'a str>,
    ) -> anyhow::Result<Self> {
        match (state == crate::a2a::TaskState::Working, waiting) {
            (true, waiting) => Ok(Self::Working { waiting }),
            (false, None) => Ok(Self::Other(state)),
            (false, Some(_)) => anyhow::bail!("waiting requires state working"),
        }
    }

    fn state(self) -> crate::a2a::TaskState {
        match self {
            Self::Working { .. } => crate::a2a::TaskState::Working,
            Self::Other(state) => state,
        }
    }

    fn waiting(self) -> Option<&'a str> {
        match self {
            Self::Working { waiting } => waiting,
            Self::Other(_) => None,
        }
    }
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
/// `unknown task` if we hold no record for `task_id`; otherwise a
/// serialize/broadcast failure.
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
    let Some(peer) = app.tasks.get(task_id).map(|rec| rec.peer.clone()) else {
        return Err(anyhow::anyhow!("unknown task '{task_id}'"));
    };
    broadcast_task_artifact(
        BroadcastTaskArtifactParams {
            mesh,
            author,
            peer: &peer,
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

/// Seal a directed payload `body` to `to`'s published X25519 key. A directed
/// frame is **never** sent in plaintext — if the recipient's card / seal key has
/// not replicated yet, this errors rather than leaking the body. The Ed25519
/// frame signature (added later, over this ciphertext) authenticates the sender.
///
/// # Errors
/// The recipient's seal key is not yet known (cards still propagating), or the
/// seal operation fails.
pub(crate) fn seal_directed(
    state: &EventLoopState,
    to: &Nickname,
    body: &MessageBody,
) -> anyhow::Result<MessageBody> {
    let doc = state.doc(Channel::Meta).to_json();
    let key = card::peer_seal_key(&doc, to).ok_or_else(|| {
        // A peer whose card did not publish says why in its own entry. Without that, the sender
        // waits for cards that never come.
        match doc
            .pointer(&format!("/peers/{to}/card_error"))
            .and_then(serde_json::Value::as_str)
        {
            Some(cause) => {
                anyhow::anyhow!("cannot seal to '{to}': its card is not published ({cause})")
            }
            None => anyhow::anyhow!(
                "cannot seal to '{to}': its encryption key is not known yet (cards still propagating)"
            ),
        }
    })?;
    habilis_network::protocol::seal_to_body(&key, body.as_str()).map_err(anyhow::Error::from)
}

/// A worker-emitted status leg's identity, addressee, task, state/note, and
/// the task registry the frame is ingested into — grouped so
/// `broadcast_task_status` stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct BroadcastTaskStatusParams<'caller> {
    pub(crate) mesh: &'caller MeshId,
    pub(crate) author: &'caller Nickname,
    pub(crate) peer: &'caller Nickname,
    pub(crate) task_id: &'caller crate::a2a::TaskId,
    pub(crate) leg: WorkerLeg<'caller>,
    pub(crate) note: Option<&'caller str>,
    pub(crate) app: &'caller mut A2aApp,
}

/// The status leg a worker emits: its state and note, the order value that
/// tells it apart from the other legs of its second, and the waiting reason.
fn worker_status_leg(leg: StatusLegParams<'_>) -> crate::a2a::TaskStatusUpdate {
    let StatusLegParams {
        mesh,
        task_id,
        state,
        note,
        waiting,
    } = leg;
    let metadata =
        status_update_params::status_metadata(waiting, status_update_params::leg_order_ms());
    let update =
        status_update_params::StatusUpdateParams::new(task_id, state).with_metadata(metadata);
    let update = match note {
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
    state: crate::a2a::TaskState,
    note: Option<&'caller str>,
    waiting: Option<&'caller str>,
}

/// A worker-emitted `TaskStatusUpdate` (`a2a status`): compose the A2A status
/// payload and push it to `peer` fire-and-forget.
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
        peer,
        task_id,
        leg,
        note,
        app,
    } = params;
    let update = worker_status_leg(StatusLegParams {
        mesh,
        task_id,
        state: leg.state(),
        note,
        waiting: leg.waiting(),
    });
    let plain = status_update_params::payload_body(&update)?;
    let sealed = seal_directed(state, peer, &plain)?;
    let kind = MessageKind::App {
        tag: wire::app_tag(wire::STATUS),
        to: Some(peer.clone()),
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
    let payload = status_update_params::compose_msg(mesh, text.as_str());
    let payload_id = MessageId::try_new(payload.message_id.as_str())
        .expect("an a2a message id is a valid frame id");
    let plain = status_update_params::payload_body(&payload)?;
    let sealed = seal_directed(state, peer, &plain)?;
    let kind = MessageKind::App {
        tag: wire::app_tag(wire::MSG),
        to: Some(peer.clone()),
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
    pub(crate) peer: &'caller Nickname,
    pub(crate) task_id: &'caller crate::a2a::TaskId,
    pub(crate) text: &'caller str,
    pub(crate) file: Option<FileRef>,
    pub(crate) app: &'caller mut A2aApp,
}

/// A worker-emitted `TaskArtifactUpdate` (`a2a artifact`): compose the A2A
/// artifact payload and push it to `peer` fire-and-forget.
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
        peer,
        task_id,
        text,
        file,
        app,
    } = params;
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
    let sealed = seal_directed(state, peer, &plain)?;
    let kind = MessageKind::App {
        tag: wire::app_tag(wire::ARTIFACT),
        to: Some(peer.clone()),
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
    let relay_transport = parsed.config().transport.relay_transport;
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
            .with_password(password.cloned()),
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

/// The id a call's blob group is named by, and whether that group is new.
/// A follow-up on a task we hold joins that task's group. Any other call has
/// no task of ours to reap its blob (a first call has none yet, and a follow-up
/// on an unknown task may be rejected or accepted by the worker), so it owns a
/// group under its own id: a retry's file must not share a group with the
/// timed-out call that it replaces.
fn call_blob_group(
    task_id: Option<crate::a2a::TaskId>,
    tasks: &HashMap<crate::a2a::TaskId, TaskRecord>,
) -> (crate::a2a::TaskId, bool) {
    match task_id {
        Some(task_id) if tasks.contains_key(&task_id) => (task_id, false),
        _ => (crate::a2a::TaskId::random(), true),
    }
}

/// The blob group a call owns until a task adopts it.
pub(crate) struct CallBlob {
    pub(crate) group: ContentId,
    /// Whether the call creates the task, rather than follows up on one.
    pub(crate) opens_task: bool,
}

/// Offload `file` and put its `Part.url` first among the parts of the
/// `SendMessage` in `params` (`a2a call --file`). An empty text part — the
/// `--text`-less sugar — is dropped, as `build_offload_parts` drops it.
///
/// A follow-up's blob on a task we hold joins its task's content group, so the
/// task sweep reaps it. Any other call's blob goes in a fresh group, which is
/// returned: the caller owns its eviction. A first call has no task id yet —
/// the worker mints it.
///
/// # Errors
/// When `params` carries no A2A `message`, or the offload fails.
pub(crate) async fn attach_call_file(
    params: &mut serde_json::Value,
    attachment: CallFile<'_>,
    state: &EventLoopState,
    app: &mut A2aApp,
) -> anyhow::Result<Option<CallBlob>> {
    let CallFile { mesh, author, file } = attachment;
    let slot = params
        .get_mut("message")
        .ok_or_else(|| anyhow::anyhow!("--file needs a SendMessage call with a message"))?;
    let mut message: crate::a2a::Message = serde_json::from_value(slot.take())?;
    let opens_task = message.task_id.is_none();
    let (group_id, fresh_group) = call_blob_group(message.task_id.clone(), &app.tasks);
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
    Ok(fresh_group.then(|| CallBlob {
        group: ContentId::new(group_id.as_str()),
        opens_task,
    }))
}

/// One outbound directed leg's wire frame/bytes plus its echo twin and
/// cap-tracking flag — the part [`send_directed_leg`] retains/delivers, grouped
/// so the state/sender/out handles stay their own leading params.
struct DirectedLegPart<'source> {
    app: &'source mut A2aApp,
    echo_as: EchoAs,
    message: &'source Verified,
    bytes: WireFrame,
    echo: Option<&'source Authored>,
    content: bool,
}

/// Broadcast (or buffer, while unmeshed) one fully-built task leg, retaining
/// **content** legs for anti-entropy. `echo` carries the plaintext twin whose
/// operator line surfaces (`None` for the raw shards of a split leg, which
/// commit silently). Errors if the unmeshed buffer is full.
async fn send_directed_leg(
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &emission::Output,
    part: DirectedLegPart<'_>,
) -> anyhow::Result<Option<PendingSend>> {
    let DirectedLegPart {
        app,
        message,
        bytes,
        echo,
        content,
        echo_as,
    } = part;
    if state.is_meshed() {
        // Meshed: a leg enters our log for anti-entropy only once it was sent
        // (see `A2aApp::unsent_legs`). Directed legs go unicast, broadcasts
        // gossip (see `transport::deliver`).
        return match habilis_network::ops::deliver(message, bytes, state, sender).await? {
            Delivered::Done => {
                retain_leg(state, message, content);
                if let Some(echo) = echo {
                    print_echo(out, echo, echo_as);
                }
                Ok(None)
            }
            Delivered::Pending(pending) => {
                let pending = if content {
                    retain_when_sent(app, message.clone(), pending)
                } else {
                    pending
                };
                let pending = match echo {
                    Some(echo) => echo_when_sent(out, echo.clone(), echo_as, vec![pending]),
                    None => vec![pending],
                };
                Ok(pending.into_iter().next())
            }
            _ => Err(anyhow::anyhow!("unsupported delivery outcome")),
        };
    } else if state.pending_outbound_mut().push((message.clone(), bytes)) {
        // Buffered until the meshed edge flushes it through the send decision.
        retain_leg(state, message, content);
        if let Some(echo) = echo {
            print_echo(out, echo, echo_as);
        }
    } else {
        tracing::warn!("pending outbound buffer full; outbound message dropped");
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; message dropped"
        ));
    }
    Ok(None)
}

/// Retain a task leg locally. Content legs retain; the beat doesn't. The echo
/// is the caller's, and it waits for the delivery verdict.
fn retain_leg(state: &mut EventLoopState, message: &Verified, content: bool) {
    if content {
        retain_outbound(state, message);
    }
}

/// Queue `msg` for the message log, behind the verdict of its send. The handle is replaced by
/// one that gives the same verdict.
fn retain_when_sent(app: &mut A2aApp, message: Verified, pending: PendingSend) -> PendingSend {
    let (sent_tx, sent_rx) = oneshot::channel();
    let (verdict_tx, verdict_rx) = oneshot::channel();
    app.unsent_legs.push(UnsentLeg {
        frames: vec![message],
        cache: None,
        sent: sent_rx,
    });
    tokio::spawn(async move {
        let verdict = settle(vec![pending]).await;
        let _ = sent_tx.send(verdict.is_ok());
        let _ = verdict_tx.send(verdict);
    });
    verdict_rx
}

/// Queue the shards of one leg for the message log and the shard cache, as one group: they
/// enter only when every shard was sent. One shard in the log lets anti-entropy and
/// `shard/repair` rebuild a leg whose send failed. `pending` holds the handle of each shard,
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
    tokio::spawn(async move {
        let verdict = settle(pending).await;
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

/// Move the own legs that were sent into the message log and the shard cache, and forget the
/// legs whose send failed.
pub(crate) fn retain_sent_legs(app: &mut A2aApp, state: &mut EventLoopState) {
    for leg in take_sent_legs(app) {
        for frame in &leg.frames {
            retain_outbound(state, frame);
        }
        if let Some((group, frames)) = leg.cache {
            state.shard_cache_mut().insert(group, frames);
        }
    }
}

/// Print `echo` once the sends in `pending` all succeed, and give the verdict on
/// to the caller. A failed send prints nothing: the sender must not read a
/// message that never arrived as sent. With nothing pending the verdict is known
/// now, and the echo prints at once.
fn echo_when_sent(
    out: &emission::Output,
    echo: Authored,
    echo_as: EchoAs,
    pending: Vec<PendingSend>,
) -> Vec<PendingSend> {
    if pending.is_empty() {
        print_echo(out, &echo, echo_as);
        return pending;
    }
    let out = out.clone();
    let (verdict_tx, verdict_rx) = oneshot::channel();
    tokio::spawn(async move {
        let verdict = settle(pending).await;
        if verdict.is_ok() {
            print_echo(&out, &echo, echo_as);
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
pub(crate) fn ingest_own_leg(app: &mut A2aApp, message: &Message, task_id: &crate::a2a::TaskId) {
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
    tokio::spawn(async move {
        let verdict = settle(pending).await;
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
        let sealed = match seal_directed(state, &ticket.author, &body) {
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
                Some(ticket.author.clone()),
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
    pub(crate) peer: Nickname,
    pub(crate) method: &'caller str,
    pub(crate) params: serde_json::Value,
    pub(crate) timeout: std::time::Duration,
    pub(crate) responder: A2aResponder,
    /// `a2a call --file`: attached only once the peer check passes, so a call
    /// that cannot go out spools nothing.
    pub(crate) file: Option<FileRef>,
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
    let party_to_a_task = app
        .tasks
        .values()
        .any(|rec| rec.peer == peer && !rec.state.is_terminal());
    if !party_to_a_task && !state.peers().contains(peer.as_str()) {
        responder.send_response(&rpc_error(-32602, &format!("unknown peer '{peer}'")));
        return;
    }
    let blob = match file {
        None => None,
        Some(file) => {
            let attachment = CallFile { mesh, author, file };
            match attach_call_file(&mut params, attachment, state, app).await {
                Ok(blob) => blob,
                Err(error) => {
                    responder.send_response(&rpc_error(-32602, &error.to_string()));
                    return;
                }
            }
        }
    };
    let opens_task = blob.as_ref().is_some_and(|blob| blob.opens_task);
    let blob_group = blob.map(|blob| blob.group);
    if let Some(message_id) = params["message"]["messageId"].as_str() {
        app.note_sent_ref(message_id, &peer);
    }
    let envelope = serde_json::json!({ "method": method, "params": params });
    let Ok(body) = MessageBody::try_new(&envelope.to_string()) else {
        app.orphan_blob(blob_group);
        responder.send_response(&rpc_error(
            -32602,
            "request params contain control characters",
        ));
        return;
    };
    // Seal the request to the addressee — a relay forwards it but cannot read it.
    let body = match seal_directed(state, &peer, &body) {
        Ok(sealed) => sealed,
        Err(error) => {
            app.orphan_blob(blob_group);
            responder.send_response(&rpc_error(-32603, &error.to_string()));
            return;
        }
    };
    // The a2a layer mints a UUID rpc id; the engine correlates on the generic
    // `CorrId` carried by the app frame.
    let corr = wire::corr_id(crate::a2a::A2aRpcId::random().as_str());
    let kind = MessageKind::App {
        tag: wire::app_tag(wire::REQ),
        to: Some(peer.clone()),
        corr: Some(corr.clone()),
    };
    // Clamp before adding to `Instant`: an unclamped client `timeout_secs` would
    // overflow the platform `Instant` and panic the event loop.
    let max_timeout = std::time::Duration::from_secs(A2A_CALL_MAX_TIMEOUT_SECS);
    if timeout > max_timeout {
        tracing::warn!(
            requested_secs = timeout.as_secs(),
            capped_secs = A2A_CALL_MAX_TIMEOUT_SECS,
            "a2a call timeout clamped to the maximum"
        );
    }
    let deadline = tokio::time::Instant::now() + timeout.min(max_timeout);
    if let Some(unregistered) = app.register_a2a_waiter(A2aWaiter {
        corr,
        peer,
        deadline,
        responder,
        blob_group,
        opens_task,
    }) {
        unregistered.send_response(&rpc_error(-32603, "too many in-flight a2a calls"));
        return;
    }
    // Directed request over the shared RPC sender: unicast per frame,
    // transparently splitting a large sealed body into shard frames.
    if let Err(error) = send_directed_rpc(
        DirectedRpcParams {
            mesh,
            author,
            kind,
            body,
        },
        state,
        sender,
    )
    .await
    {
        if method == served::READER_GET {
            tracing::debug!(target: "agent_gossip::a2a", %error, "reader query send failed");
        } else {
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
/// frames are plumbing (never logged for anti-entropy); the shards reassemble
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
    // The RPC body is already sealed (base58, ~1.37x the input) — gate on the
    // sealed ceiling so the caller-facing limit stays `MAX_LOGICAL_BODY_BYTES`.
    if body.as_str().len() > MAX_SEALED_BODY_BYTES {
        return Err(anyhow::anyhow!(
            "rpc body too large: {} sealed bytes exceeds the input ceiling \
             ({MAX_LOGICAL_BODY_BYTES} raw bytes); use the blob channel for files",
            body.as_str().len()
        ));
    }
    let group = ShardGroup::from_uuid_str(crate::a2a::A2aRpcId::random().as_str())
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
        if series.is_cached() {
            cache_frames.push(bytes.clone());
        }
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
    crate::a2a::TaskState,
    Option<String>,
    Option<String>,
    Sender<anyhow::Result<Authored>>,
);

async fn session_task_status(
    request: SessionTaskStatus,
    scope: SessionScope<'_>,
    app: &mut A2aApp,
    state: &mut EventLoopState,
) -> bool {
    let SessionTaskStatus(task_id, task_state, note, waiting, response) = request;
    let outcome = emit_task_status(
        TaskStatusParams {
            mesh: scope.mesh,
            author: scope.author,
            task_id: &task_id,
            task_state,
            note: note.as_deref(),
            waiting: waiting.as_deref(),
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
            peer,
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
    let _ = response.send(outcome.map(|_| ()).map_err(anyhow::Error::from));
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
            state: task_state,
            note,
            waiting,
            response,
        } => {
            let fields = SessionTaskStatus(task_id, task_state, note, waiting, response);
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
            let _ = response.send(state.doc(Channel::State).to_json());
            false
        }
        SessionRequest::MetaMerge { merge, response } => {
            session_merge(SessionMerge(Channel::Meta, merge, response), scope, state).await
        }
        SessionRequest::MetaGet { response } => {
            let _ = response.send(state.doc(Channel::Meta).to_json());
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
mod call_blob_group_tests {
    use std::collections::HashMap;
    use std::time::Instant;

    use habilis_network::protocol::Nickname;

    use crate::a2a::task_record::{AdoptInitiatorParams, adopt_initiator};
    use crate::a2a::{TaskId, TaskState};

    /// A follow-up on a task this node does not hold has no task of ours to
    /// reap its blob, whether the worker rejects it or holds the task itself:
    /// the call must own a fresh group for the waiter to evict.
    #[test]
    fn a_follow_up_on_an_unknown_task_owns_a_fresh_group() {
        let tasks = HashMap::new();
        let unknown = TaskId::random();

        let (group_id, fresh_group) = super::call_blob_group(Some(unknown.clone()), &tasks);

        assert_ne!(
            group_id, unknown,
            "the group is not the content group of a task"
        );
        assert!(fresh_group, "the blob has no task to be reaped with");
    }

    /// A follow-up on a held task joins that task's group, which the task sweep
    /// reaps.
    #[test]
    fn a_follow_up_on_a_known_task_joins_its_group() {
        let mut tasks = HashMap::new();
        let known = TaskId::random();
        adopt_initiator(
            &mut tasks,
            AdoptInitiatorParams {
                task_id: &known,
                peer: &Nickname::from("bob"),
                task_state: TaskState::Working,
                now: Instant::now(),
            },
        );

        let (task_id, fresh_group) = super::call_blob_group(Some(known.clone()), &tasks);

        assert_eq!(task_id, known, "the known task is the one found");
        assert!(!fresh_group, "the task sweep reaps the blob");
    }

    /// Two timed-out follow-ups on the same unknown task are two calls with two
    /// files. Each owns its group, so the eviction of one cannot take the blob
    /// of a retry that the task now holds.
    #[test]
    fn a_retried_follow_up_on_an_unknown_task_gets_its_own_group() {
        let tasks = HashMap::new();
        let unknown = TaskId::random();

        let (first, _) = super::call_blob_group(Some(unknown.clone()), &tasks);
        let (retry, _) = super::call_blob_group(Some(unknown), &tasks);

        assert_ne!(first, retry, "a retry gets a different group");
    }

    #[test]
    fn a_first_call_owns_a_fresh_group() {
        let (_, fresh_group) = super::call_blob_group(None, &HashMap::new());

        assert!(fresh_group, "a first call owns a fresh group");
    }
}

#[cfg(test)]
mod echo_when_sent_tests {
    use habilis_network::protocol::message::Authored;
    use habilis_network::protocol::{MeshId, Message, MessageBody, MessageKind, Nickname};
    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::oneshot;

    use super::{EchoAs, PendingSend, echo_when_sent};
    use crate::a2a::wire;
    use crate::emission;
    use crate::output_event::OutputEvent;

    fn echo() -> Authored {
        echo_raw()
            .sign(&habilis_network::protocol::Identity::generate())
            .authored()
    }

    fn echo_raw() -> Message {
        Message::new_frame(
            &MeshId::from("test"),
            &Nickname::from("alice"),
            MessageKind::app_to(wire::MSG, Nickname::from("bob"), None),
            MessageBody::try_new("hi").expect("valid body"),
        )
    }

    fn echoes(events: &mut UnboundedReceiver<OutputEvent>) -> usize {
        std::iter::from_fn(|| events.try_recv().ok())
            .filter(|event| matches!(event, OutputEvent::Message { is_self: true, .. }))
            .count()
    }

    #[tokio::test]
    async fn a_delivered_send_prints_the_echo() {
        let (out, mut events) = emission::capture_events::capture_events();
        let (verdict_tx, verdict_rx): (_, PendingSend) = oneshot::channel();

        let pending = echo_when_sent(&out, echo(), EchoAs::Chat, vec![verdict_rx]);
        verdict_tx.send(Ok(())).expect("receiver is held");
        let verdict = pending.into_iter().next().expect("verdict handle");
        verdict.await.expect("verdict").expect("ok");

        assert_eq!(echoes(&mut events), 1, "one echo is sent");
    }

    #[tokio::test]
    async fn a_failed_send_prints_no_echo_and_passes_the_error_on() {
        let (out, mut events) = emission::capture_events::capture_events();
        let (verdict_tx, verdict_rx): (_, PendingSend) = oneshot::channel();

        let pending = echo_when_sent(&out, echo(), EchoAs::Chat, vec![verdict_rx]);
        verdict_tx
            .send(Err(anyhow::anyhow!("unicast dial failed")))
            .expect("receiver is held");
        let verdict = pending
            .into_iter()
            .next()
            .expect("verdict handle")
            .await
            .expect("verdict");

        assert!(verdict.is_err(), "the caller still sees the failure");
        assert_eq!(echoes(&mut events), 0, "no echo follows a failure");
    }
}

#[cfg(test)]
mod sharded_leg_tests {
    use habilis_network::protocol::message::Verified;
    use habilis_network::protocol::{MeshId, Message, MessageBody, MessageKind, Nickname};
    use tokio::sync::oneshot;

    use super::{PendingSend, queue_sharded_leg, settle, take_sent_legs};
    use crate::a2a::a2a_app::{A2aApp, UnsentLeg};
    use crate::a2a::wire;

    fn shard(text: &str) -> Verified {
        shard_raw(text).sign(&habilis_network::protocol::Identity::generate())
    }

    fn shard_raw(text: &str) -> Message {
        Message::new_frame(
            &MeshId::from("test"),
            &Nickname::from("alice"),
            MessageKind::app_to(wire::MSG, Nickname::from("bob"), None),
            MessageBody::try_new(text).expect("valid body"),
        )
    }

    fn verdict(outcome: anyhow::Result<()>) -> PendingSend {
        let (verdict_tx, verdict_rx) = oneshot::channel();
        verdict_tx.send(outcome).expect("receiver is held");
        verdict_rx
    }

    #[tokio::test]
    async fn a_sharded_leg_with_one_failed_shard_enters_no_log() {
        let mut app = A2aApp::new();
        let pending = vec![
            Some(verdict(Ok(()))),
            Some(verdict(Err(anyhow::anyhow!("dial failed")))),
        ];

        let handles = queue_sharded_leg(&mut app, vec![shard("a"), shard("b")], None, pending);
        assert!(settle(handles).await.is_err(), "the leg failed");

        let logged: usize = take_sent_legs(&mut app)
            .iter()
            .map(|leg| leg.frames.len())
            .sum();
        assert_eq!(logged, 0, "no shard of a failed leg may enter the log");
    }

    #[tokio::test]
    async fn a_leg_whose_send_task_ended_without_a_verdict_is_dropped() {
        let mut app = A2aApp::new();
        let (sent_tx, sent) = oneshot::channel::<bool>();
        drop(sent_tx);
        app.unsent_legs.push(UnsentLeg {
            frames: vec![shard("a")],
            cache: None,
            sent,
        });

        assert!(
            take_sent_legs(&mut app).is_empty(),
            "nothing enters the log"
        );
        assert!(app.unsent_legs.is_empty(), "the leg is not kept waiting");
    }

    #[tokio::test]
    async fn a_sharded_leg_whose_shards_were_all_sent_enters_the_log_whole() {
        let mut app = A2aApp::new();
        let pending = vec![None, Some(verdict(Ok(())))];

        let handles = queue_sharded_leg(&mut app, vec![shard("a"), shard("b")], None, pending);
        assert!(settle(handles).await.is_ok(), "the leg was sent");

        let logged: usize = take_sent_legs(&mut app)
            .iter()
            .map(|leg| leg.frames.len())
            .sum();
        assert_eq!(logged, 2, "two shards are logged");
    }
}

#[cfg(test)]
mod waiting_tests {
    use super::WorkerLeg;
    use crate::a2a::TaskState;

    /// The shared sender refuses a reason on any state but `working`, so a
    /// library caller cannot leave `mesh:waiting` on an `input-required` task.
    #[test]
    fn a_waiting_reason_needs_the_working_state() {
        assert!(
            WorkerLeg::parse(TaskState::Working, Some("asks its user")).is_ok(),
            "a worker leg may wait on its user while working"
        );
        assert!(
            WorkerLeg::parse(TaskState::InputRequired, None).is_ok(),
            "an input-required leg parses without a wait"
        );
        for state in [
            TaskState::InputRequired,
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Canceled,
            TaskState::Rejected,
        ] {
            let error = WorkerLeg::parse(state, Some("asks its user"))
                .err()
                .expect("a reason with a non-working state must be refused");
            assert_eq!(
                error.to_string(),
                "waiting requires state working",
                "waiting requires a working state"
            );
        }
    }
}

#[cfg(test)]
mod status_leg_tests {
    use habilis_network::protocol::MeshId;

    use super::{StatusLegParams, worker_status_leg};
    use crate::a2a::TaskState;
    use crate::a2a::status_update_params::{status_order, status_waiting};

    /// The worker's status leg carries the order value next to the waiting
    /// reason. Without the order value the initiator cannot tell two legs of one
    /// second apart.
    #[test]
    fn a_worker_status_leg_carries_its_order_value_and_reason() {
        let mesh = MeshId::from("test");
        let task_id = crate::a2a::TaskId::random();
        let leg = worker_status_leg(StatusLegParams {
            mesh: &mesh,
            task_id: &task_id,
            state: TaskState::Working,
            note: None,
            waiting: Some("asks its user"),
        });
        assert!(status_order(&leg).is_some(), "the leg has no mesh:order_ms");
        assert_eq!(
            status_waiting(&leg).as_deref(),
            Some("asks its user"),
            "the status waiting reason is kept"
        );
    }
}
