//! [`send_broadcast`] is the single source of truth for the chat send path —
//! the IPC `Msg` command and the typed in-process `SessionRequest` both funnel
//! through it so they cannot drift.

use std::time::Instant;

use bytes::Bytes;

use crate::a2a::app::A2aApp;
use crate::a2a::session::SessionRequest;
use crate::a2a::wire;
use crate::output;
use agent_habilis_mesh::embed::EventLoopState;
use agent_habilis_mesh::ops::MeshSender;
use agent_habilis_mesh::protocol::Identity;
use agent_habilis_mesh::protocol::{
    AppFrameParams, AppTag, Channel, CorrId, MeshId, Message, MessageBody, MessageId, MessageKind,
    Nickname, Shard, ShardGroup,
};
use agent_habilis_mesh::util::consts::{
    LOGGED_SHARD_GROUP_MAX_TOTAL, MAX_LOGICAL_BODY_BYTES, MAX_MESSAGE_SIZE, MAX_SEALED_BODY_BYTES,
    MAX_SHARD_TOTAL,
};

/// Retain a just-built outbound message in the local log (pruning the
/// fork/DAG indexes on any eviction) and write the dev log. The operator
/// echo is the caller's responsibility — it differs by kind
/// (`print_message_ex` for `Msg`, `print_task` for `Task`).
fn retain_outbound(state: &mut EventLoopState, msg: &Message) {
    if let Some(evicted) = state.message_log_mut().push(msg.clone()) {
        let evicted_hash = evicted.content_hash_hex();
        state.forget_hash(&evicted_hash);
        if let Some(seq) = evicted.seq {
            state.forget_msg_seq(&evicted.pubkey, seq, &evicted_hash);
        }
    }
    agent_habilis_mesh::util::logging::log_out(msg);
}

/// Commit a just-built outbound `Msg` into local state: advance the per-author
/// hash chain (`seq`/`prev`) and the DAG tips, retain it in the message log
/// (pruning the fork/DAG indexes on eviction), write the dev log, and — when
/// `echo` — print the operator line. The raw shards of a split body commit
/// silently (`echo == false`); only the reassembled logical message is echoed.
fn commit_outbound_part(
    state: &mut EventLoopState,
    msg: &Message,
    out: &output::Output,
    echo: bool,
) {
    let hash = msg.content_hash_hex();
    state.advance_chain(hash.clone());
    state.note_dag(hash, &msg.parents, msg.timestamp);
    if echo {
        out.print_message_ex(msg, true);
    }
    retain_outbound(state, msg);
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
    prev: Option<String>,
    parents: Vec<String>,
}

/// The identity + payload of one outbound chat frame — the fields `build_msg`
/// shares with [`synthesize_logical_msg`], grouped so the chain-stamp/shard/
/// signer args stay their own params.
struct ChatFrameParams<'a> {
    mesh: &'a MeshId,
    author: &'a Nickname,
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
) -> Message {
    let ChatFrameParams {
        mesh,
        author,
        body,
        id,
    } = frame;
    let mut msg = Message::new_app(
        mesh,
        author,
        AppFrameParams {
            tag: AppTag::from(wire::BROADCAST),
            to: None,
            corr: None,
            body,
        },
    );
    if let Some(id) = id {
        msg = msg.with_id(id);
    }
    msg.with_chain(stamp.seq, stamp.prev)
        .with_parents(stamp.parents)
        .with_shard(shard)
        .signed(signer)
}

/// One outbound wire frame plus its echo flag — the part
/// [`send_msg_part`] commits/delivers, grouped so the state/sender/out handles
/// stay their own leading params.
struct OutboundPart<'a> {
    msg: &'a Message,
    bytes: Bytes,
    echo: bool,
}

/// Broadcast (or buffer, while unmeshed) one fully-built `Msg` and commit it to
/// the per-author chain + log. `echo` gates the operator print so the raw shards
/// of a split body commit silently. Errors if the unmeshed buffer is full.
async fn send_msg_part(
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &output::Output,
    part: OutboundPart<'_>,
) -> anyhow::Result<()> {
    let OutboundPart { msg, bytes, echo } = part;
    if state.is_meshed() {
        commit_outbound_part(state, msg, out, echo);
        // Single send decision: a directed message goes point-to-point over
        // unicast, a broadcast over gossip (see `transport::deliver`).
        agent_habilis_mesh::ops::deliver(msg, bytes, state, sender).await?;
    } else if state
        .pending_outbound_mut()
        .push((msg.clone(), bytes.clone()))
    {
        // Buffered until the meshed edge flushes it through the send decision.
        commit_outbound_part(state, msg, out, echo);
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

/// The reassembled logical chat frame to echo + return after a sharded send.
/// Its `id` is the shard `group` — which the chat path pins to the payload's
/// A2A `messageId` — so the sender and every receiver name the reassembled
/// body identically. Unsigned / unchained — a local view, not a wire message
/// (the shards carry the wire bytes and the chain entries).
fn synthesize_logical_msg(
    mesh: &MeshId,
    author: &Nickname,
    body: MessageBody,
    group: &ShardGroup,
) -> Message {
    let mut msg = Message::new_app(
        mesh,
        author,
        AppFrameParams {
            tag: AppTag::from(wire::BROADCAST),
            to: None,
            corr: None,
            body,
        },
    );
    msg.id = MessageId::new(group.as_str()).expect("a shard group is a valid message id");
    msg
}

/// The identity + text of one outbound chat broadcast — grouped so
/// `send_broadcast` stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct BroadcastParams<'a> {
    pub(crate) mesh: &'a MeshId,
    pub(crate) author: &'a Nickname,
    pub(crate) text: MessageBody,
}

/// Build, sign, log and gossip-broadcast one outbound chat message. The
/// single source of truth for the send path: the CLI socket's IPC `Msg`
/// command and the typed in-process `SessionRequest::Broadcast` both funnel
/// through here so they cannot drift. `text` is the operator/agent input; it
/// is wrapped here — and only here — into the A2A payload the wire carries
/// (see [`crate::a2a::gossip::compose_broadcast`]), so role/context/extension
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
#[expect(
    clippy::too_many_lines,
    reason = "one straight-line compose/sign/split/broadcast pipeline shared by every caller; splitting would scatter a single sequential path across helpers"
)]
pub(crate) async fn send_broadcast(
    params: BroadcastParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
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
    let signer = state.identity().clone();
    let payload = crate::a2a::gossip::compose_broadcast(mesh, text.as_str());
    let payload_id =
        MessageId::new(payload.message_id.as_str()).expect("an a2a message id is a valid frame id");
    let body = crate::a2a::gossip::payload_body(&payload)?;
    // On a passworded mesh, seal the chat body so a relay or a captured frame
    // can't read it. Everything downstream (dedup, the per-author chain, the
    // message log, anti-entropy) operates on the sealed frame; only the local
    // echo + the returned Message carry plaintext.
    let wire_body = match state.broadcast_key() {
        Some(key) => agent_habilis_mesh::ops::doc::encrypt_body(&body, key)?,
        None => body.clone(),
    };
    let encrypted = state.broadcast_key().is_some();
    // Read the chain position once: every frame this call emits — the single
    // frame, the probe, and each shard — stamps the *same* position, and only
    // `note_own` advances it afterwards.
    let (chain_seq, chain_prev) = {
        let (seq, prev) = state.chain_head();
        (seq, prev.map(str::to_owned))
    };
    // Fast path: the whole payload in one frame, its id the A2A messageId.
    let single = build_msg(
        ChatFrameParams {
            mesh,
            author,
            body: wire_body.clone(),
            id: Some(payload_id.clone()),
        },
        ChainStamp {
            seq: chain_seq,
            prev: chain_prev.clone(),
            parents: state.dag_parents(),
        },
        None,
        &signer,
    );
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        let bytes = Bytes::from(single.serialize()?);
        let id = single.id.clone();
        if encrypted {
            // Commit + gossip the sealed frame silently, then echo/return a
            // plaintext-bodied view so the operator/agent sees the text.
            send_msg_part(
                state,
                sender,
                out,
                OutboundPart {
                    msg: &single,
                    bytes,
                    echo: false,
                },
            )
            .await?;
            let mut plain = single.clone();
            plain.body = body;
            out.print_message_ex(&plain, true);
            return Ok((id, plain));
        }
        send_msg_part(
            state,
            sender,
            out,
            OutboundPart {
                msg: &single,
                bytes,
                echo: true,
            },
        )
        .await?;
        return Ok((id, single));
    }
    // Too big: split the payload across shard-tagged frames. Each shard is an
    // ordinary chained frame with its own minted id, retained for
    // anti-entropy; the *group* carries the A2A messageId, so the reassembled
    // logical frame — the only thing echoed and returned — is named by it.
    let group = ShardGroup::from_uuid_str(payload.message_id.as_str())
        .expect("an a2a message id is a valid shard group");
    let max_parts = MAX_SHARD_TOTAL;
    // Size the probe against the *largest* shard's envelope, not the first's. Each
    // later shard chains off the prior, so even from genesis (empty `prev`/`parents`)
    // every shard after the first carries a 64-char `prev` and one parent hash.
    // Worst-case both: a full-length `prev` and as many parent hashes as the first
    // shard could hold (its current tips, never fewer than the one-hash link later
    // shards carry). A 64-char zero string stands in for any content hash.
    let hash_stub = "0".repeat(64);
    let probe_parents = vec![hash_stub.clone(); state.dag_parents().len().max(1)];
    let probe = build_msg(
        ChatFrameParams {
            mesh,
            author,
            body: MessageBody::new(String::new()).expect("empty body is valid"),
            id: None,
        },
        ChainStamp {
            seq: chain_seq,
            prev: Some(hash_stub),
            parents: probe_parents,
        },
        Some(Shard {
            group: group.clone(),
            idx: max_parts - 1,
            total: max_parts,
        }),
        &signer,
    );
    let budget = shard_body_budget(&probe);
    // Split the *wire* (sealed, when encrypted) body — shards carry chunks of
    // the ciphertext envelope that reassemble into it; the receiver decrypts the
    // reassembled body. The echoed/returned logical still carries plaintext.
    let chunks = split_body(wire_body.as_str(), budget).ok_or_else(|| {
        // Report the wire length that is actually being split: when encrypted
        // it is the ~1.4x-inflated sealed body, which is why a chat that fits
        // passwordless can exceed the shard cap here.
        let note = if encrypted {
            " (sealing inflates the body ~1.4x)"
        } else {
            ""
        };
        anyhow::anyhow!(
            "message too large: a {}-byte wire body needs more than {MAX_SHARD_TOTAL} shards{note}",
            wire_body.as_str().len()
        )
    })?;
    let total = u32::try_from(chunks.len()).expect("chunk count is bounded by MAX_SHARD_TOTAL");
    // Atomic admission while unmeshed: all shards or none, so a half-buffered body
    // (which could never reassemble) never reaches peers.
    if !state.is_meshed() && state.pending_outbound_mut().remaining() < chunks.len() {
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; multipart message dropped"
        ));
    }
    // A big group's shards skip the message log (`shard_fits_log`), so peers
    // can only heal a lost shard via the `shard/repair` RPC — served from this
    // sender-side frame cache. Small groups heal through anti-entropy instead,
    // which re-sends a directed shard point-to-point to its addressee.
    let mut cache_frames = Vec::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        let shard = Shard {
            group: group.clone(),
            idx: u32::try_from(idx).expect("idx is bounded by MAX_SHARD_TOTAL"),
            total,
        };
        let chunk_body = MessageBody::new(*chunk).expect("a substring of a valid body is valid");
        let msg = build_msg(
            ChatFrameParams {
                mesh,
                author,
                body: chunk_body,
                id: None,
            },
            ChainStamp {
                seq: chain_seq,
                prev: chain_prev.clone(),
                parents: state.dag_parents(),
            },
            Some(shard),
            &signer,
        );
        let bytes = Bytes::from(msg.serialize()?);
        if total > LOGGED_SHARD_GROUP_MAX_TOTAL {
            cache_frames.push(bytes.clone());
        }
        send_msg_part(
            state,
            sender,
            out,
            OutboundPart {
                msg: &msg,
                bytes,
                echo: false,
            },
        )
        .await?;
    }
    if !cache_frames.is_empty() {
        state.shard_cache_mut().insert(group.clone(), cache_frames);
    }
    let logical = synthesize_logical_msg(mesh, author, body, &group);
    out.print_message_ex(&logical, true);
    Ok((logical.id.clone(), logical))
}

/// One worker-pushed task frame's identity, target-kind, payload/echo bodies,
/// and cap-tracking flag, plus the in-flight task registry it's ingested
/// into — grouped so `broadcast_directed_frame` stays within the argument budget
/// alongside its state/sender/out handles.
struct DirectedFrameParams<'a> {
    mesh: &'a MeshId,
    author: &'a Nickname,
    /// `None` for a leg that belongs to no task — a msg. Its
    /// presence is also what decides whether the coarse task machine is
    /// advanced for our own leg.
    task_id: Option<&'a crate::a2a::TaskId>,
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
    app: &'a mut A2aApp,
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
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
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
    let single = single.signed(&signer);
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        let bytes = Bytes::from(single.serialize()?);
        let id = single.id.clone();
        // The wire frame's directed body is sealed to the addressee, which we
        // cannot decrypt; surface *and ingest* the self-echo from a plaintext
        // twin instead (same id, never signed or sent).
        let echo = Message::new_frame(mesh, author, kind, echo_body).with_id(id.clone());
        send_directed_leg(
            state,
            sender,
            out,
            DirectedLegPart {
                msg: &single,
                bytes,
                echo: Some(&echo),
                content,
                echo_as,
            },
        )
        .await?;
        if let Some(task_id) = task_id {
            ingest_own_leg(app, &echo, task_id);
        }
        return Ok((id, single));
    }
    // Only content legs are ever large enough to split; the beat is a tiny
    // liveness widget and is never retained/reassembled.
    if !content {
        return Err(anyhow::anyhow!("task beat leg too large to send"));
    }
    let logical_id = single.id.clone();
    // The shard group carries the logical id, so the reassembled view is
    // named identically on both ends.
    let group =
        ShardGroup::from_uuid_str(logical_id.as_str()).expect("a frame id is a valid shard group");
    let max_parts = MAX_SHARD_TOTAL;
    let probe = Message::new_frame(
        mesh,
        author,
        kind.clone(),
        MessageBody::new(String::new()).expect("empty body is valid"),
    )
    .with_shard(Some(Shard {
        group: group.clone(),
        idx: max_parts - 1,
        total: max_parts,
    }))
    .signed(&signer);
    let budget = shard_body_budget(&probe);
    let chunks = split_body(payload_body.as_str(), budget).ok_or_else(|| {
        anyhow::anyhow!(
            "task leg too large: a {}-byte body needs more than {MAX_SHARD_TOTAL} shards",
            payload_body.as_str().len()
        )
    })?;
    let total = u32::try_from(chunks.len()).expect("chunk count is bounded by MAX_SHARD_TOTAL");
    if !state.is_meshed() && state.pending_outbound_mut().remaining() < chunks.len() {
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; multipart task leg dropped"
        ));
    }
    // A big group's shards skip the message log, so peers heal a lost shard via
    // the `shard/repair` RPC — served from this sender-side frame cache.
    let mut cache_frames = Vec::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        let shard = Shard {
            group: group.clone(),
            idx: u32::try_from(idx).expect("idx is bounded by MAX_SHARD_TOTAL"),
            total,
        };
        let chunk_body = MessageBody::new(*chunk).expect("a substring of a valid body is valid");
        let msg = Message::new_frame(mesh, author, kind.clone(), chunk_body)
            .with_shard(Some(shard))
            .signed(&signer);
        let bytes = Bytes::from(msg.serialize()?);
        if total > LOGGED_SHARD_GROUP_MAX_TOTAL {
            cache_frames.push(bytes.clone());
        }
        send_directed_leg(
            state,
            sender,
            out,
            DirectedLegPart {
                msg: &msg,
                bytes,
                echo: None,
                content,
                echo_as,
            },
        )
        .await?;
    }
    if !cache_frames.is_empty() {
        state.shard_cache_mut().insert(group.clone(), cache_frames);
    }
    // Echo + ingest the logical leg once; the echo is the plaintext twin
    // (the logical body is sealed).
    let logical =
        Message::new_frame(mesh, author, kind.clone(), payload_body).with_id(logical_id.clone());
    let echo = Message::new_frame(mesh, author, kind, echo_body).with_id(logical_id);
    print_echo(out, &echo, echo_as);
    if let Some(task_id) = task_id {
        ingest_own_leg(app, &echo, task_id);
    }
    Ok((logical.id.clone(), logical))
}

/// Print a directed leg's self-echo down the surface its kind belongs to.
fn print_echo(out: &output::Output, echo: &Message, echo_as: EchoAs) {
    match echo_as {
        EchoAs::Task => out.print_task(echo, true),
        EchoAs::Chat => out.print_message_ex(echo, true),
    }
}

/// A worker-emitted status leg's identity, task, state/note, and the task
/// registry it's resolved (and later ingested) against — grouped so
/// `emit_task_status` stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct TaskStatusParams<'a> {
    pub(crate) mesh: &'a MeshId,
    pub(crate) author: &'a Nickname,
    pub(crate) task_id: &'a crate::a2a::TaskId,
    pub(crate) task_state: crate::a2a::TaskState,
    pub(crate) note: Option<&'a str>,
    pub(crate) app: &'a mut A2aApp,
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
/// `unknown task` if we hold no record for `task_id`; otherwise a
/// serialize/broadcast failure.
pub(crate) async fn emit_task_status(
    params: TaskStatusParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &output::Output,
) -> anyhow::Result<Message> {
    let TaskStatusParams {
        mesh,
        author,
        task_id,
        task_state,
        note,
        app,
    } = params;
    let Some(peer) = app.tasks.get(task_id).map(|rec| rec.peer.clone()) else {
        return Err(anyhow::anyhow!("unknown task '{task_id}'"));
    };
    broadcast_task_status(
        BroadcastTaskStatusParams {
            mesh,
            author,
            peer: &peer,
            task_id,
            task_state,
            note,
            app,
        },
        state,
        sender,
        out,
    )
    .await
    .map(|(_id, msg)| msg)
}

/// A worker-emitted artifact leg's identity, task, result text/file, and the
/// task registry it's resolved (and later ingested) against — grouped so
/// `emit_task_artifact` stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct TaskArtifactEmitParams<'a> {
    pub(crate) mesh: &'a MeshId,
    pub(crate) author: &'a Nickname,
    pub(crate) task_id: &'a crate::a2a::TaskId,
    pub(crate) text: &'a str,
    pub(crate) file: Option<FileRef>,
    pub(crate) app: &'a mut A2aApp,
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
    out: &output::Output,
) -> anyhow::Result<Message> {
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
    .map(|(_id, msg)| msg)
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
    let key = crate::a2a::card::peer_seal_key(&doc, to).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot seal to '{to}': its encryption key is not known yet (cards still propagating)"
        )
    })?;
    agent_habilis_mesh::protocol::seal_to_body(&key, body.as_str())
}

/// A worker-emitted status leg's identity, addressee, task, state/note, and
/// the task registry the frame is ingested into — grouped so
/// `broadcast_task_status` stays within the argument budget alongside its
/// state/sender/out handles.
pub(crate) struct BroadcastTaskStatusParams<'a> {
    pub(crate) mesh: &'a MeshId,
    pub(crate) author: &'a Nickname,
    pub(crate) peer: &'a Nickname,
    pub(crate) task_id: &'a crate::a2a::TaskId,
    pub(crate) task_state: crate::a2a::TaskState,
    pub(crate) note: Option<&'a str>,
    pub(crate) app: &'a mut A2aApp,
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
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
    let BroadcastTaskStatusParams {
        mesh,
        author,
        peer,
        task_id,
        task_state,
        note,
        app,
    } = params;
    let update = crate::a2a::gossip::status_update(
        mesh,
        crate::a2a::gossip::StatusUpdateParams {
            task_id,
            state: task_state,
            note,
            metadata: None,
        },
    );
    let plain = crate::a2a::gossip::payload_body(&update)?;
    let sealed = seal_directed(state, peer, &plain)?;
    let kind = MessageKind::App {
        tag: AppTag::from(wire::STATUS),
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
pub(crate) struct MsgParams<'a> {
    pub(crate) mesh: &'a MeshId,
    pub(crate) author: &'a Nickname,
    pub(crate) peer: &'a Nickname,
    pub(crate) text: MessageBody,
    pub(crate) app: &'a mut A2aApp,
}

/// Send one msg: a chat line addressed to a single peer. The
/// counterpart of [`send_broadcast`], and the single source of truth for the
/// msg send path.
///
/// Three properties, each load-bearing and none of them incidental:
///
/// - **Directed**, so [`agent_habilis_mesh::ops::deliver`] routes it unicast —
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
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
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
    let payload = crate::a2a::gossip::compose_msg(mesh, text.as_str());
    let payload_id =
        MessageId::new(payload.message_id.as_str()).expect("an a2a message id is a valid frame id");
    let plain = crate::a2a::gossip::payload_body(&payload)?;
    let sealed = seal_directed(state, peer, &plain)?;
    let kind = MessageKind::App {
        tag: AppTag::from(wire::MSG),
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
pub(crate) struct BroadcastTaskArtifactParams<'a> {
    pub(crate) mesh: &'a MeshId,
    pub(crate) author: &'a Nickname,
    pub(crate) peer: &'a Nickname,
    pub(crate) task_id: &'a crate::a2a::TaskId,
    pub(crate) text: &'a str,
    pub(crate) file: Option<FileRef>,
    pub(crate) app: &'a mut A2aApp,
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
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
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
    let update = crate::a2a::gossip::artifact_update_parts(mesh, task_id, parts);
    let plain = crate::a2a::gossip::payload_body(&update)?;
    let sealed = seal_directed(state, peer, &plain)?;
    let kind = MessageKind::App {
        tag: AppTag::from(wire::ARTIFACT),
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
struct OffloadTextParams<'a> {
    mesh: &'a MeshId,
    author: &'a Nickname,
    task_id: &'a crate::a2a::TaskId,
    text: &'a str,
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
    let lookups = mesh
        .as_str()
        .parse::<agent_habilis_mesh::protocol::Mesh>()
        .map_err(|error| anyhow::anyhow!("cannot resolve mesh lookups for blob offload: {error}"))?
        .lookups()
        .clone();
    // Route through the choke point so the base is validated (0700, ours) before
    // this attachment payload spool is created — bypassing it could birth the
    // shared base at a world-traversable 0755.
    let spool = crate::ensure_mesh_runtime_dir(mesh.as_str())
        .map_err(|error| anyhow::anyhow!("cannot prepare blob spool dir: {error}"))?
        .join(format!("{author}.blobs"));
    // Every offloaded blob inherits the mesh password (if any), so a scraped
    // ticket can't be redeemed without it.
    let password = state.mesh_password();
    let ticket = agent_habilis_mesh::ops::blob::offload(
        &mut app.blob_server,
        &lookups,
        agent_habilis_mesh::ops::blob::OffloadRequest {
            path: file.path,
            spool_dir: spool,
            content_id: agent_habilis_mesh::ops::blob::ContentId::new(task_id.as_str()),
            password: password.cloned(),
        },
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

/// One outbound directed leg's wire frame/bytes plus its echo twin and
/// cap-tracking flag — the part [`send_directed_leg`] retains/delivers, grouped
/// so the state/sender/out handles stay their own leading params.
struct DirectedLegPart<'a> {
    echo_as: EchoAs,
    msg: &'a Message,
    bytes: Bytes,
    echo: Option<&'a Message>,
    content: bool,
}

/// Broadcast (or buffer, while unmeshed) one fully-built task leg, retaining
/// **content** legs for anti-entropy. `echo` carries the plaintext twin whose
/// operator line surfaces (`None` for the raw shards of a split leg, which
/// commit silently). Errors if the unmeshed buffer is full.
async fn send_directed_leg(
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &output::Output,
    part: DirectedLegPart<'_>,
) -> anyhow::Result<()> {
    let DirectedLegPart {
        msg,
        bytes,
        echo,
        content,
        echo_as,
    } = part;
    if state.is_meshed() {
        // Meshed: retain locally, then hit the wire (a transient send error
        // still leaves a content leg in our log for anti-entropy). Directed
        // legs go unicast, broadcasts gossip (see `transport::deliver`).
        retain_leg(
            state,
            out,
            RetainLegParams {
                msg,
                echo,
                content,
                echo_as,
            },
        );
        agent_habilis_mesh::ops::deliver(msg, bytes, state, sender).await?;
    } else if state
        .pending_outbound_mut()
        .push((msg.clone(), bytes.clone()))
    {
        // Buffered until the meshed edge flushes it through the send decision.
        retain_leg(
            state,
            out,
            RetainLegParams {
                msg,
                echo,
                content,
                echo_as,
            },
        );
    } else {
        tracing::warn!("pending outbound buffer full; outbound message dropped");
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; message dropped"
        ));
    }
    Ok(())
}

/// A task leg's frame plus its echo twin and cap-tracking flag — the fields
/// [`retain_leg`] needs beyond its state/out handles.
#[derive(Clone, Copy)]
struct RetainLegParams<'a> {
    msg: &'a Message,
    echo: Option<&'a Message>,
    content: bool,
    echo_as: EchoAs,
}

/// Retain a task leg locally, echoing the operator line from the plaintext
/// twin when given (`None` for the raw shards of a split leg — the wire
/// frame's directed body is sealed, so it cannot be surfaced). Content legs
/// retain; the beat doesn't.
fn retain_leg(state: &mut EventLoopState, out: &output::Output, params: RetainLegParams<'_>) {
    let RetainLegParams {
        msg,
        echo,
        content,
        echo_as,
    } = params;
    if let Some(echo) = echo {
        print_echo(out, echo, echo_as);
    }
    if content {
        retain_outbound(state, msg);
    }
}

/// Advance our own coarse task state machine for a leg we just sent. `msg`
/// must be the **plaintext twin** of the leg, never the wire frame: the wire
/// body is sealed to the addressee, so a status leg's state could not be read
/// back out of it. The `task_id` is threaded in for the same reason it always
/// was — the artifact leg never needs its body at all.
///
/// Only *skill-driven* legs may come through here. The daemon's own keepalive
/// beat must NOT — see [`crate::a2a::task::broadcast_status`].
fn ingest_own_leg(app: &mut A2aApp, msg: &Message, task_id: &crate::a2a::TaskId) {
    crate::a2a::task::ingest(
        &mut app.tasks,
        crate::a2a::task::IngestLegParams {
            frame: msg,
            task_id,
            mine: true,
            now: Instant::now(),
        },
    );
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
    let tickets = state.reassembly_mut().repair_tickets(Instant::now());
    for ticket in tickets {
        let envelope = serde_json::json!({
            "method": "shard/repair",
            "params": { "group": ticket.group.as_str(), "missing": ticket.missing },
        });
        let Ok(body) = MessageBody::new(envelope.to_string()) else {
            continue;
        };
        let sealed = match seal_directed(state, &ticket.author, &body) {
            Ok(sealed) => sealed,
            Err(error) => {
                tracing::debug!(%error, author = %ticket.author, "cannot seal shard repair request yet");
                continue;
            }
        };
        let corr = CorrId::from(crate::a2a::A2aRpcId::random().as_str());
        let frame = Message::new_app(
            mesh,
            author,
            AppFrameParams {
                tag: AppTag::from(wire::REQ),
                to: Some(ticket.author.clone()),
                corr: Some(corr),
                body: sealed,
            },
        )
        .signed(state.identity());
        agent_habilis_mesh::util::logging::log_out(&frame);
        match frame.serialize() {
            Ok(bytes) => {
                if let Err(error) =
                    agent_habilis_mesh::ops::deliver(&frame, Bytes::from(bytes), state, sender)
                        .await
                {
                    tracing::debug!(%error, "shard repair request send failed; next tick retries");
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
pub(crate) struct BroadcastA2aCallParams<'a> {
    pub(crate) mesh: &'a MeshId,
    pub(crate) author: &'a Nickname,
    pub(crate) peer: Nickname,
    pub(crate) method: &'a str,
    pub(crate) params: serde_json::Value,
    pub(crate) timeout: std::time::Duration,
    pub(crate) responder: crate::a2a::app::A2aResponder,
}

/// waiter times out via the loop's a2a-deadline arm. Fails fast (through the
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
    } = call;
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
    let envelope = serde_json::json!({ "method": method, "params": params });
    let Ok(body) = MessageBody::new(envelope.to_string()) else {
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
            responder.send_response(&rpc_error(-32603, &error.to_string()));
            return;
        }
    };
    // The a2a layer mints a UUID rpc id; the engine correlates on the generic
    // `CorrId` carried by the app frame.
    let corr = CorrId::from(crate::a2a::A2aRpcId::random().as_str());
    let kind = MessageKind::App {
        tag: AppTag::from(wire::REQ),
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
    if let Some(unregistered) = app.register_a2a_waiter(crate::a2a::app::A2aWaiter {
        corr,
        peer,
        deadline,
        responder,
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
        tracing::warn!(target: "agent_gossip::a2a", %error, "a2a request send failed");
    }
}

/// A directed RPC frame's identity, kind, and body — the fields
/// `send_directed_rpc` needs beyond its state/sender handles.
pub(crate) struct DirectedRpcParams<'a> {
    pub(crate) mesh: &'a MeshId,
    pub(crate) author: &'a Nickname,
    pub(crate) kind: MessageKind,
    pub(crate) body: MessageBody,
}

/// Send one directed RPC frame (an `App` request/response), transparently
/// splitting a sealed body too large for one frame into shard-tagged frames —
/// each an ordinary directed frame riding unicast, so the RPC plane is
/// size-transparent like chat and task legs. RPC
/// frames are plumbing (never logged for anti-entropy); the shards reassemble
/// only in the receiver's dedicated store, and a big group is served from the
/// sender-side [`ShardCache`](agent_habilis_mesh::reassembly::ShardCache)
/// through the `shard/repair` RPC.
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
    let single = Message::new_frame(mesh, author, kind.clone(), body.clone()).signed(&signer);
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        agent_habilis_mesh::util::logging::log_out(&single);
        let bytes = Bytes::from(single.serialize()?);
        return agent_habilis_mesh::ops::deliver(&single, bytes, state, sender).await;
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
    let probe = Message::new_frame(
        mesh,
        author,
        kind.clone(),
        MessageBody::new(String::new()).expect("empty body is valid"),
    )
    .with_shard(Some(Shard {
        group: group.clone(),
        idx: MAX_SHARD_TOTAL - 1,
        total: MAX_SHARD_TOTAL,
    }))
    .signed(&signer);
    let budget = shard_body_budget(&probe);
    let chunks = split_body(body.as_str(), budget).ok_or_else(|| {
        anyhow::anyhow!("rpc body too large: needs more than {MAX_SHARD_TOTAL} shards")
    })?;
    let total = u32::try_from(chunks.len()).expect("chunk count is bounded by MAX_SHARD_TOTAL");
    if !state.is_meshed() && state.pending_outbound_mut().remaining() < chunks.len() {
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; multipart rpc dropped"
        ));
    }
    let mut cache_frames = Vec::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        let chunk_body = MessageBody::new(*chunk).expect("a substring of a valid body is valid");
        let msg = Message::new_frame(mesh, author, kind.clone(), chunk_body)
            .with_shard(Some(Shard {
                group: group.clone(),
                idx: u32::try_from(idx).expect("idx is bounded by MAX_SHARD_TOTAL"),
                total,
            }))
            .signed(&signer);
        agent_habilis_mesh::util::logging::log_out(&msg);
        let bytes = Bytes::from(msg.serialize()?);
        if total > LOGGED_SHARD_GROUP_MAX_TOTAL {
            cache_frames.push(bytes.clone());
        }
        agent_habilis_mesh::ops::deliver(&msg, bytes, state, sender).await?;
    }
    if !cache_frames.is_empty() {
        state.shard_cache_mut().insert(group, cache_frames);
    }
    Ok(())
}

/// A ping round's identity plus the responder the RTT rows are delivered
/// through — grouped so `session_ping` stays within the argument budget
/// alongside its state/sender handles.
struct SessionPingParams<'a> {
    mesh: &'a MeshId,
    author: &'a Nickname,
    resp: tokio::sync::oneshot::Sender<Vec<(Nickname, u64)>>,
}

/// Arm a fresh ping round carrying the responder; the deadline-driven
/// `finalize_ping_round` delivers the RTT rows through it. Mirrors the IPC
/// `Ping` handler, which leaves `resp` unset and emits the `ping_report`
/// event instead.
async fn session_ping(
    params: SessionPingParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
) -> bool {
    let SessionPingParams { mesh, author, resp } = params;
    let now = tokio::time::Instant::now();
    state.arm_ping_round(agent_habilis_mesh::embed::PingRound {
        t1: now,
        deadline: now
            + std::time::Duration::from_secs(
                agent_habilis_mesh::runtime::tuning::ping_window_secs(),
            ),
        pongs: std::collections::HashMap::new(),
        resp: Some(resp),
    });
    agent_habilis_mesh::ops::broadcast_msg(
        sender,
        &Message::new_ping(mesh, author).signed(state.identity()),
    )
    .await;
    true
}

/// One typed in-process request plus the identity + app state it's served
/// against — grouped so `handle_session_request` stays within the argument
/// budget alongside its state/sender/output handles.
pub(crate) struct SessionRequestParams<'a> {
    pub(crate) req: SessionRequest,
    pub(crate) mesh: &'a MeshId,
    pub(crate) author: &'a Nickname,
    pub(crate) app: &'a mut A2aApp,
}

/// Handle one typed in-process [`SessionRequest`] (in-process). `Send`
/// broadcasts via the shared helper and echoes the canonical [`Message`]
/// back on the oneshot; `Poll` returns the join-horizon-filtered buffer.
/// Returns `true` if anything was broadcast so the caller can refresh
/// `last_sent_at` (mirrors `handle_ipc_command`).
#[expect(
    clippy::too_many_lines,
    reason = "a dispatch match with one arm per SessionRequest variant (mirrors handle_ipc_command)"
)]
pub(crate) async fn handle_session_request(
    params: SessionRequestParams<'_>,
    state: &mut EventLoopState,
    sender: &MeshSender,
    output: &output::Output,
) -> bool {
    let SessionRequestParams {
        req,
        mesh,
        author,
        app,
    } = params;
    match req {
        SessionRequest::Broadcast { body, resp } => {
            let outcome = send_broadcast(
                BroadcastParams {
                    mesh,
                    author,
                    text: body,
                },
                state,
                sender,
                output,
            )
            .await
            .map(|(_id, msg)| msg);
            let sent_ok = outcome.is_ok();
            let _ = resp.send(outcome);
            sent_ok
        }
        SessionRequest::Msg { to, body, resp } => {
            let outcome = send_msg(
                MsgParams {
                    mesh,
                    author,
                    peer: &to,
                    text: body,
                    app,
                },
                state,
                sender,
                output,
            )
            .await
            .map(|(_id, msg)| msg);
            let sent_ok = outcome.is_ok();
            let _ = resp.send(outcome);
            sent_ok
        }
        SessionRequest::Poll { after, long, resp } => {
            // Same policy as the CLI/IPC `Poll` arm: respond now if events are
            // buffered, else (with `long`) park a typed waiter the loop
            // fulfills/expires. A parked waiter broadcasts nothing → `false`.
            app.surfaced
                .poll_or_register(crate::a2a::surfaced::PollOrRegisterParams {
                    after,
                    long,
                    now: tokio::time::Instant::now(),
                    responder: crate::a2a::surfaced::PollResponder::Typed(resp),
                });
            false
        }
        SessionRequest::TaskStatus {
            task_id,
            state: task_state,
            note,
            resp,
        } => {
            let outcome = emit_task_status(
                TaskStatusParams {
                    mesh,
                    author,
                    task_id: &task_id,
                    task_state,
                    note: note.as_deref(),
                    app,
                },
                state,
                sender,
                output,
            )
            .await;
            let sent_ok = outcome.is_ok();
            let _ = resp.send(outcome);
            sent_ok
        }
        SessionRequest::TaskArtifact {
            task_id,
            text,
            file,
            resp,
        } => {
            let outcome = emit_task_artifact(
                TaskArtifactEmitParams {
                    mesh,
                    author,
                    task_id: &task_id,
                    text: &text,
                    file,
                    app,
                },
                state,
                sender,
                output,
            )
            .await;
            let sent_ok = outcome.is_ok();
            let _ = resp.send(outcome);
            sent_ok
        }
        SessionRequest::Peers { resp } => {
            let _ = resp.send(state.roster_snapshot());
            false
        }
        SessionRequest::StateMerge { merge, resp } => {
            let outcome = agent_habilis_mesh::ops::broadcast_state_merge(
                state,
                agent_habilis_mesh::ops::StateMergeParams {
                    mesh,
                    author,
                    merge,
                    sender,
                    sink: output,
                    channel: Channel::State,
                    surface: true,
                },
            )
            .await;
            let sent = outcome.is_ok();
            let _ = resp.send(outcome.map(|_| ()));
            sent
        }
        SessionRequest::StateGet { resp } => {
            let _ = resp.send(state.doc(Channel::State).to_json());
            false
        }
        SessionRequest::MetaMerge { merge, resp } => {
            let outcome = agent_habilis_mesh::ops::broadcast_state_merge(
                state,
                agent_habilis_mesh::ops::StateMergeParams {
                    mesh,
                    author,
                    merge,
                    sender,
                    sink: output,
                    channel: Channel::Meta,
                    surface: true,
                },
            )
            .await;
            let sent = outcome.is_ok();
            let _ = resp.send(outcome.map(|_| ()));
            sent
        }
        SessionRequest::MetaGet { resp } => {
            let _ = resp.send(state.doc(Channel::Meta).to_json());
            false
        }
        SessionRequest::Ping { resp } => {
            session_ping(SessionPingParams { mesh, author, resp }, state, sender).await
        }
        SessionRequest::A2aCall {
            peer,
            method,
            params: rpc_params,
            timeout,
            resp,
        } => {
            broadcast_a2a_call(
                BroadcastA2aCallParams {
                    mesh,
                    author,
                    peer,
                    method: &method,
                    params: rpc_params,
                    timeout,
                    responder: crate::a2a::app::A2aResponder::Typed(resp),
                },
                state,
                app,
                sender,
            )
            .await;
            true
        }
        // Raw injection (adversarial only): broadcast the bytes verbatim, no
        // signing or chain stamping — a malicious/crafted message on the wire.
        #[cfg(feature = "adversarial")]
        SessionRequest::InjectRaw { bytes } => {
            let _ = sender.broadcast(bytes).await;
            true
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
        SessionRequest::ReassemblyStats { resp } => {
            let _ = resp.send(state.reassembly_mut().stats());
            false
        }
    }
}

#[cfg(test)]
mod split_body_tests {
    use super::{escaped_char_len, split_body};
    use agent_habilis_mesh::util::consts::MAX_SHARD_TOTAL;

    #[test]
    fn escaped_len_counts_json_escapes() {
        assert_eq!(escaped_char_len('a'), 1);
        assert_eq!(escaped_char_len('"'), 2);
        assert_eq!(escaped_char_len('\\'), 2);
        assert_eq!(escaped_char_len('\n'), 2);
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
        assert!(chunks.len() <= MAX_SHARD_TOTAL as usize);
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
        assert!(chunks.iter().all(|chunk| !chunk.is_empty()));
    }

    #[test]
    fn refuses_a_body_needing_too_many_parts() {
        // One byte per shard would need one shard per byte; a body one byte past
        // `MAX_SHARD_TOTAL` therefore needs more than the cap and is refused.
        let body = "x".repeat(MAX_SHARD_TOTAL as usize + 1);
        assert!(split_body(&body, 1).is_none());
    }
}
