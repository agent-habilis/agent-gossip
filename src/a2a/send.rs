//! [`broadcast_message`] is the single source of truth for the chat send path —
//! the IPC `Msg` command, the typed in-process `SessionRequest`, and stdin all
//! funnel through it so they cannot drift.

use std::time::Instant;

use bytes::Bytes;

use crate::a2a::app::A2aApp;
use crate::a2a::session::SessionRequest;
use crate::a2a::wire;
use crate::output;
use agent_habilis_mesh::daemon::state::EventLoopState;
use agent_habilis_mesh::protocol::identity::Identity;
use agent_habilis_mesh::protocol::{
    AppTag, Channel, CorrId, Message, MessageBody, MessageId, MessageKind, Nickname, Shard,
    ShardGroup, MeshId,
};
use agent_habilis_mesh::transport::MeshSender;
use agent_habilis_mesh::util::consts::{
    LOGGED_SHARD_GROUP_MAX_TOTAL, MAX_LOGICAL_BODY_BYTES, MAX_MESSAGE_SIZE, MAX_SEALED_BODY_BYTES,
    MAX_SHARD_TOTAL,
};

/// Process one line of interactive stdin as a mesh broadcast (A2A is
/// point-to-point, so directed 1:1 is a task, not a chat line) — validate the
/// body, then delegate to `broadcast_message` so the send (and its
/// oversize/serialize error handling) is identical to the IPC and embed paths.
pub(crate) async fn handle_stdin_line(
    text: &str,
    sender: &MeshSender,
    mesh: &MeshId,
    author: &Nickname,
    state: &mut EventLoopState,
    out: &output::Output,
) {
    out.clear_input_line();
    if text.is_empty() {
        return;
    }
    let body = match MessageBody::new(text) {
        Ok(body) => body,
        Err(error) => {
            out.report_error(&error);
            return;
        }
    };
    match broadcast_message(mesh, author, body, state, sender, out).await {
        Ok(_) => state.last_sent_at = Instant::now(),
        Err(error) => out.report_error(&error),
    }
}

/// Retain a just-built outbound message in the local log (pruning the
/// fork/DAG indexes on any eviction) and write the dev log. The operator
/// echo is the caller's responsibility — it differs by kind
/// (`print_message_ex` for `Msg`, `print_task` for `Task`).
fn retain_outbound(state: &mut EventLoopState, msg: &Message) {
    if let Some(evicted) = state.message_log.push(msg.clone()) {
        let evicted_hash = evicted.content_hash_hex();
        state.forget_hash(&evicted_hash);
        if let Some(seq) = evicted.seq {
            state.forget_msg_seq(&evicted.pubkey, seq, &evicted_hash);
        }
    }
    agent_habilis_mesh::logging::messages::log_out(msg);
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
    state.self_seq += 1;
    state.self_prev = Some(hash.clone());
    state.note_dag(hash, &msg.parents, msg.timestamp);
    if echo {
        out.print_message_ex(msg, true);
    }
    retain_outbound(state, msg);
}

/// Headroom subtracted from a shard's body budget so the first shard's measured
/// envelope still covers later shards, whose `seq` may have grown a digit or two.
const PART_BUDGET_MARGIN: usize = 16;

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

/// Build, chain-stamp, shard-tag and sign one outbound chat frame (without
/// serializing — the caller measures or serializes). `id` pins the frame id:
/// the payload's A2A `messageId` for a single-frame body (`None` keeps the
/// minted id — shards of a split body keep their own ids; the *group* carries
/// the A2A id there).
fn build_msg(
    mesh: &MeshId,
    author: &Nickname,
    body: MessageBody,
    id: Option<MessageId>,
    stamp: ChainStamp,
    shard: Option<Shard>,
    signer: &Identity,
) -> Message {
    let mut msg = Message::new_app(mesh, author, AppTag::from(wire::MSG), None, None, body);
    if let Some(id) = id {
        msg = msg.with_id(id);
    }
    msg.with_chain(stamp.seq, stamp.prev)
        .with_parents(stamp.parents)
        .with_shard(shard)
        .signed(signer)
}

/// Broadcast (or buffer, while unmeshed) one fully-built `Msg` and commit it to
/// the per-author chain + log. `echo` gates the operator print so the raw shards
/// of a split body commit silently. Errors if the unmeshed buffer is full.
async fn send_msg_part(
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &output::Output,
    msg: &Message,
    bytes: Bytes,
    echo: bool,
) -> anyhow::Result<()> {
    if state.meshed {
        commit_outbound_part(state, msg, out, echo);
        // Single send decision: a directed message goes point-to-point over
        // unicast when the addressee is dialable, else gossip (see `unicast`).
        agent_habilis_mesh::unicast::deliver(msg, bytes, state, sender).await?;
    } else if state.pending_outbound.push(bytes.clone()) {
        // Buffered for gossip; mirror to the spool now so a never-meshed spool
        // daemon still exports it (on a later mesh, flush re-broadcasts and the
        // spool tee is a no-op — the file already exists).
        sender.spool(&bytes);
        commit_outbound_part(state, msg, out, echo);
    } else {
        // Full buffer: the frame reaches no plane, so it must NOT be spooled —
        // the reported drop has to match reality.
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
    let mut msg = Message::new_app(mesh, author, AppTag::from(wire::MSG), None, None, body);
    msg.id = MessageId::new(group.as_str()).expect("a shard group is a valid message id");
    msg
}

/// Build, sign, log and gossip-broadcast one outbound chat message. The
/// single source of truth for the send path: the CLI socket's IPC `Msg`
/// command and the typed in-process `SessionRequest::Send` both funnel
/// through here so they cannot drift. `text` is the operator/agent input; it
/// is wrapped here — and only here — into the A2A payload the wire carries
/// (see [`crate::a2a::gossip::chat_message`]), so role/context/extension
/// stamping cannot drift between callers. A payload too large for one frame
/// is transparently split into `shard`-tagged messages the receiver
/// reassembles; the returned [`Message`] is the whole logical frame either
/// way, its id pinned to the payload's A2A `messageId`.
///
/// # Errors
/// Propagates a [`Message::serialize`] failure and a gossip broadcast error,
/// errors if the unmeshed pending-outbound buffer is full, and refuses a body
/// that would need more than [`MAX_SHARD_TOTAL`] shards.
pub(crate) async fn broadcast_message(
    mesh: &MeshId,
    author: &Nickname,
    text: MessageBody,
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
    let signer = state.identity.clone();
    let payload = crate::a2a::gossip::chat_message(mesh, text.as_str());
    let payload_id =
        MessageId::new(payload.message_id.as_str()).expect("an a2a message id is a valid frame id");
    let body = crate::a2a::gossip::payload_body(&payload)?;
    // On a passworded mesh, seal the chat body so a relay or a captured frame
    // can't read it. Everything downstream (dedup, the per-author chain, the
    // message log, anti-entropy) operates on the sealed frame; only the local
    // echo + the returned Message carry plaintext.
    let wire_body = match state.broadcast_key.as_deref() {
        Some(key) => agent_habilis_mesh::daemon::state_doc::encrypt_body(&body, key)?,
        None => body.clone(),
    };
    let encrypted = state.broadcast_key.is_some();
    // Fast path: the whole payload in one frame, its id the A2A messageId.
    let single = build_msg(
        mesh,
        author,
        wire_body.clone(),
        Some(payload_id.clone()),
        ChainStamp {
            seq: state.self_seq,
            prev: state.self_prev.clone(),
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
            send_msg_part(state, sender, out, &single, bytes, false).await?;
            let mut plain = single.clone();
            plain.body = body;
            out.print_message_ex(&plain, true);
            return Ok((id, plain));
        }
        send_msg_part(state, sender, out, &single, bytes, true).await?;
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
        mesh,
        author,
        MessageBody::new(String::new()).expect("empty body is valid"),
        None,
        ChainStamp {
            seq: state.self_seq,
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
    if !state.meshed && state.pending_outbound.remaining() < chunks.len() {
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; multipart message dropped"
        ));
    }
    // A big group's shards skip the message log (`shard_fits_log`), so peers
    // can only heal a lost shard via the `shard/repair` RPC — served from this
    // sender-side frame cache. Small groups heal through anti-entropy instead.
    let mut cache_frames = Vec::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        let shard = Shard {
            group: group.clone(),
            idx: u32::try_from(idx).expect("idx is bounded by MAX_SHARD_TOTAL"),
            total,
        };
        let chunk_body = MessageBody::new(*chunk).expect("a substring of a valid body is valid");
        let msg = build_msg(
            mesh,
            author,
            chunk_body,
            None,
            ChainStamp {
                seq: state.self_seq,
                prev: state.self_prev.clone(),
                parents: state.dag_parents(),
            },
            Some(shard),
            &signer,
        );
        let bytes = Bytes::from(msg.serialize()?);
        if total > LOGGED_SHARD_GROUP_MAX_TOTAL {
            cache_frames.push(bytes.clone());
        }
        send_msg_part(state, sender, out, &msg, bytes, false).await?;
    }
    if !cache_frames.is_empty() {
        state.shard_cache.insert(group.clone(), cache_frames);
    }
    let logical = synthesize_logical_msg(mesh, author, body, &group);
    out.print_message_ex(&logical, true);
    Ok((logical.id.clone(), logical))
}

/// Broadcast one already-composed task frame (a worker's `TaskStatusUpdate`
/// or `TaskArtifactUpdate`), sharding a large body, retaining **content**
/// legs for anti-entropy, echoing the operator line, and advancing our own
/// coarse task machine. `content` is false for a liveness beat (never
/// retained/reassembled). Fire-and-forget worker→initiator push — the A2A
/// streaming plane over gossip.
///
/// # Errors
/// Propagates a [`Message::serialize`] failure, a gossip broadcast error, and
/// a full unmeshed pending-outbound buffer.
#[expect(
    clippy::too_many_arguments,
    reason = "a task-frame emit threads mesh/author identity, the target + task id + payload, and the state/app/sender/output it broadcasts through"
)]
async fn broadcast_task_frame(
    mesh: &MeshId,
    author: &Nickname,
    task_id: &crate::a2a::TaskId,
    kind: MessageKind,
    payload_body: MessageBody,
    content: bool,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    sender: &MeshSender,
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
    let signer = state.identity.clone();
    // Fast path: the whole leg in one frame.
    let single =
        Message::new_frame(mesh, author, kind.clone(), payload_body.clone()).signed(&signer);
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        let bytes = Bytes::from(single.serialize()?);
        let id = single.id.clone();
        send_task_leg(state, sender, out, &single, bytes, true, content).await?;
        ingest_own_leg(app, &single, task_id, out);
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
    if !state.meshed && state.pending_outbound.remaining() < chunks.len() {
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
        send_task_leg(state, sender, out, &msg, bytes, false, content).await?;
    }
    if !cache_frames.is_empty() {
        state.shard_cache.insert(group.clone(), cache_frames);
    }
    // Echo + ingest the logical leg once (one content leg toward the cap).
    let logical = Message::new_frame(mesh, author, kind, payload_body).with_id(logical_id);
    out.print_task(&logical, true);
    ingest_own_leg(app, &logical, task_id, out);
    Ok((logical.id.clone(), logical))
}

/// Worker-emit a `TaskStatusUpdate` for a task we're serving: resolve the
/// other party from the task record and push the status to it. A progress note
/// like `"35/100"` on a non-terminal `working` state rides as a beat fraction.
///
/// # Errors
/// `unknown task` if we hold no record for `task_id`; otherwise a
/// serialize/broadcast failure.
#[expect(
    clippy::too_many_arguments,
    reason = "a task-frame emit threads mesh/author identity, the target + task id + payload, and the state/app/sender/output it broadcasts through"
)]
pub(crate) async fn emit_task_status(
    mesh: &MeshId,
    author: &Nickname,
    task_id: &crate::a2a::TaskId,
    task_state: crate::a2a::TaskState,
    note: Option<&str>,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    sender: &MeshSender,
    out: &output::Output,
) -> anyhow::Result<Message> {
    let Some(peer) = app.tasks.get(task_id).map(|rec| rec.peer.clone()) else {
        return Err(anyhow::anyhow!("unknown task '{task_id}'"));
    };
    broadcast_task_status(
        mesh, author, &peer, task_id, task_state, note, state, app, sender, out,
    )
    .await
    .map(|(_id, msg)| msg)
}

/// Worker-emit a `TaskArtifactUpdate` (the result) for a task we're serving.
///
/// # Errors
/// `unknown task` if we hold no record for `task_id`; otherwise a
/// serialize/broadcast failure.
#[expect(
    clippy::too_many_arguments,
    reason = "threads mesh/author identity, the task id + result text/file, and the state/app/sender/output it broadcasts through"
)]
pub(crate) async fn emit_task_artifact(
    mesh: &MeshId,
    author: &Nickname,
    task_id: &crate::a2a::TaskId,
    text: &str,
    file: Option<agent_habilis_mesh::blob::FileRef>,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    sender: &MeshSender,
    out: &output::Output,
) -> anyhow::Result<Message> {
    let Some(peer) = app.tasks.get(task_id).map(|rec| rec.peer.clone()) else {
        return Err(anyhow::anyhow!("unknown task '{task_id}'"));
    };
    broadcast_task_artifact(
        mesh, author, &peer, task_id, text, file, state, app, sender, out,
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
    let doc = state.meta_doc.to_json();
    let key = crate::a2a::card::peer_seal_key(&doc, to).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot seal to '{to}': its encryption key is not known yet (cards still propagating)"
        )
    })?;
    agent_habilis_mesh::protocol::seal::seal_to_body(&key, body.as_str())
}

/// A worker-emitted `TaskStatusUpdate` (`a2a status`): compose the A2A status
/// payload and push it to `peer` fire-and-forget.
///
/// # Errors
/// Propagates a serialize/broadcast failure.
#[expect(
    clippy::too_many_arguments,
    reason = "a task-frame emit threads mesh/author identity, the target + task id + payload, and the state/app/sender/output it broadcasts through"
)]
pub(crate) async fn broadcast_task_status(
    mesh: &MeshId,
    author: &Nickname,
    peer: &Nickname,
    task_id: &crate::a2a::TaskId,
    task_state: crate::a2a::TaskState,
    note: Option<&str>,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    sender: &MeshSender,
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
    let update = crate::a2a::gossip::status_update(mesh, task_id, task_state, note, None);
    let body = seal_directed(state, peer, &crate::a2a::gossip::payload_body(&update)?)?;
    let kind = MessageKind::App {
        tag: AppTag::from(wire::STATUS),
        to: Some(peer.clone()),
        corr: None,
    };
    broadcast_task_frame(
        mesh, author, task_id, kind, body, true, state, app, sender, out,
    )
    .await
}

/// A worker-emitted `TaskArtifactUpdate` (`a2a artifact`): compose the A2A
/// artifact payload and push it to `peer` fire-and-forget.
///
/// # Errors
/// Propagates a serialize/broadcast failure.
#[expect(
    clippy::too_many_arguments,
    reason = "a task-frame emit threads mesh/author identity, the target + task id + payload, and the state/app/sender/output it broadcasts through"
)]
pub(crate) async fn broadcast_task_artifact(
    mesh: &MeshId,
    author: &Nickname,
    peer: &Nickname,
    task_id: &crate::a2a::TaskId,
    text: &str,
    file: Option<agent_habilis_mesh::blob::FileRef>,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    sender: &MeshSender,
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
    let parts = build_offload_parts(mesh, author, task_id, text, file, state, app).await?;
    let update = crate::a2a::gossip::artifact_update_parts(mesh, task_id, parts);
    let body = seal_directed(state, peer, &crate::a2a::gossip::payload_body(&update)?)?;
    let kind = MessageKind::App {
        tag: AppTag::from(wire::ARTIFACT),
        to: Some(peer.clone()),
        corr: None,
    };
    broadcast_task_frame(
        mesh, author, task_id, kind, body, true, state, app, sender, out,
    )
    .await
}

/// Resolve the parts of a `--file`-bearing leg: with no file, a single text
/// part (today's behavior); with a file, offload it over the blob channel into a
/// `Part.url` reference (+ the text part when non-empty). Lazily binds this
/// peer's blob server on the first offload. The heavy read+hash runs off the
/// event loop inside `blob::url_part`.
async fn build_offload_parts(
    mesh: &MeshId,
    author: &Nickname,
    task_id: &crate::a2a::TaskId,
    text: &str,
    file: Option<agent_habilis_mesh::blob::FileRef>,
    state: &EventLoopState,
    app: &mut A2aApp,
) -> anyhow::Result<Vec<crate::a2a::Part>> {
    let Some(file) = file else {
        return Ok(vec![crate::a2a::Part::text(text)]);
    };
    let lookups = mesh
        .as_str()
        .parse::<agent_habilis_mesh::protocol::mesh::Mesh>()
        .map_err(|error| anyhow::anyhow!("cannot resolve mesh lookups for blob offload: {error}"))?
        .lookups()
        .clone();
    // Route through the choke point so the base is validated (0700, ours) before
    // this attachment payload spool is created — bypassing it could birth the
    // shared base at a world-traversable 0755.
    let spool = agent_habilis_mesh::util::ensure_mesh_runtime_dir(mesh.as_str())
        .map_err(|error| anyhow::anyhow!("cannot prepare blob spool dir: {error}"))?
        .join(format!("{author}.blobs"));
    // Every offloaded blob inherits the mesh password (if any), so a scraped
    // ticket can't be redeemed without it.
    let password = state.mesh_password.clone();
    let offload = agent_habilis_mesh::blob::url_part(
        file,
        &mut app.blob_server,
        &lookups,
        spool,
        agent_habilis_mesh::blob::ContentId::new(task_id.as_str()),
        password,
    )
    .await?;
    let part = crate::a2a::Part {
        url: Some(offload.url),
        filename: offload.filename,
        media_type: offload.media_type,
        ..crate::a2a::Part::default()
    };
    let mut parts = vec![part];
    if !text.is_empty() {
        parts.push(crate::a2a::Part::text(text));
    }
    Ok(parts)
}

/// Broadcast (or buffer, while unmeshed) one fully-built task leg, retaining
/// **content** legs for anti-entropy. `echo` gates the operator print so the
/// raw shards of a split leg commit silently. Errors if the unmeshed buffer
/// is full.
async fn send_task_leg(
    state: &mut EventLoopState,
    sender: &MeshSender,
    out: &output::Output,
    msg: &Message,
    bytes: Bytes,
    echo: bool,
    content: bool,
) -> anyhow::Result<()> {
    if state.meshed {
        // Meshed: retain locally, then hit the wire (a transient send error
        // still leaves a content leg in our log for anti-entropy). Unicast when
        // the addressee is dialable, else gossip (see `unicast::deliver`).
        retain_leg(state, msg, out, echo, content);
        agent_habilis_mesh::unicast::deliver(msg, bytes, state, sender).await?;
    } else if state.pending_outbound.push(bytes.clone()) {
        // Buffered for gossip; mirror to the spool only on a successful buffer
        // so a reported drop matches reality.
        sender.spool(&bytes);
        retain_leg(state, msg, out, echo, content);
    } else {
        tracing::warn!("pending outbound buffer full; outbound message dropped");
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; message dropped"
        ));
    }
    Ok(())
}

/// Retain a task leg locally, echoing the operator line only when `echo`
/// (false for the raw shards of a split leg). Content legs retain; the beat
/// doesn't.
fn retain_leg(
    state: &mut EventLoopState,
    msg: &Message,
    out: &output::Output,
    echo: bool,
    content: bool,
) {
    if echo {
        out.print_task(msg, true);
    }
    if content {
        retain_outbound(state, msg);
    }
}

/// Advance our own coarse task state machine for a leg we just sent and warn
/// once if a content leg pushed the task past its whole-task cap. The `task_id`
/// is threaded in rather than parsed from `msg`: our own echoed directed leg
/// carries a *sealed* body, so parsing it would fail.
fn ingest_own_leg(
    app: &mut A2aApp,
    msg: &Message,
    task_id: &crate::a2a::TaskId,
    out: &output::Output,
) {
    if crate::a2a::task::ingest(&mut app.tasks, msg, task_id, true, Instant::now()) {
        out.info(&format!(
            "task exceeded {} messages; wrap it up",
            agent_habilis_mesh::util::consts::TASK_CONTENT_CAP
        ));
        tracing::warn!("task content cap exceeded");
    }
}

/// Client side of the gossip A2A request/response: mint an `rpc_id`, park a
/// waiter (keyed by `rpc_id` + `peer`, with `timeout`), and broadcast an
/// `A2aReq` directed at `peer` carrying the JSON-RPC `{method, params}`. The
/// peer serves the safe method set and replies with an `A2aResp` that the
/// receive path routes into the waiter (`app.fulfill_a2a_waiter`); the
/// Ask the authors of our stalled big (unlogged) partial groups to re-send the
/// missing shards — the repair half of big-group reliability (the author half is
/// `state.shard_cache`, served by `resend_cached_shards`). Runs on the app tick;
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
    let tickets = state.reassembly.repair_tickets(Instant::now());
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
            AppTag::from(wire::REQ),
            Some(ticket.author.clone()),
            Some(corr),
            sealed,
        )
        .signed(&state.identity);
        agent_habilis_mesh::logging::messages::log_out(&frame);
        match frame.serialize() {
            Ok(bytes) => {
                if let Err(error) = agent_habilis_mesh::unicast::deliver(
                    &frame,
                    Bytes::from(bytes),
                    state,
                    sender,
                )
                .await
                {
                    tracing::debug!(%error, "shard repair request send failed; next tick retries");
                }
            }
            Err(error) => tracing::debug!(%error, "shard repair request serialize failed"),
        }
    }
}

/// waiter times out via the loop's a2a-deadline arm. Fails fast (through the
/// responder) when `peer` isn't a current participant or the waiter registry
/// is full — no silent park.
#[expect(
    clippy::too_many_arguments,
    reason = "the client call carries mesh/author identity, the target + RPC method/params/timeout, the transport-specific responder, and the state + app + sender it parks and broadcasts through"
)]
pub(crate) async fn broadcast_a2a_call(
    mesh: &MeshId,
    author: &Nickname,
    peer: Nickname,
    method: &str,
    params: serde_json::Value,
    timeout: std::time::Duration,
    responder: crate::a2a::app::A2aResponder,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    sender: &MeshSender,
) {
    let rpc_error = |code: i64, message: &str| {
        serde_json::json!({ "error": { "code": code, "message": message } }).to_string()
    };
    // Fast-fail an unknown participant, EXCEPT when we already hold a task with
    // this peer: a follow-up / read / cancel into a live task must survive a
    // brief roster flap (the waiter's timeout is the feedback if the peer is
    // truly gone), so it isn't wedged. Task creation (we hold no task with the
    // peer yet) and any read/cancel to a peer we share no task with still
    // require the peer present — method-agnostic, so `SendStreamingMessage`
    // creates gate the same as `SendMessage`.
    let party_to_a_task = app.tasks.values().any(|rec| rec.peer == peer);
    if !party_to_a_task && !state.participants.contains(peer.as_str()) {
        responder.send_response(&rpc_error(-32602, &format!("unknown participant '{peer}'")));
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
    let max_timeout = std::time::Duration::from_secs(
        agent_habilis_mesh::util::consts::A2A_CALL_MAX_TIMEOUT_SECS,
    );
    if timeout > max_timeout {
        tracing::warn!(
            requested_secs = timeout.as_secs(),
            capped_secs = agent_habilis_mesh::util::consts::A2A_CALL_MAX_TIMEOUT_SECS,
            "a2a call timeout clamped to the maximum"
        );
    }
    let deadline = tokio::time::Instant::now() + timeout.min(max_timeout);
    if let Some(unregistered) = app.register_a2a_waiter(corr, peer, deadline, responder) {
        unregistered.send_response(&rpc_error(-32603, "too many in-flight a2a calls"));
        return;
    }
    // Directed request over the shared RPC sender: unicast → circuit → gossip per
    // frame, transparently splitting a large sealed body into shard frames.
    if let Err(error) = send_directed_rpc(mesh, author, kind, body, state, sender).await {
        tracing::warn!(target: "agent_mesh::gossip", %error, "a2a request send failed");
    }
}

/// Send one directed RPC frame (an `App` request/response), transparently
/// splitting a sealed body too large for one frame into shard-tagged frames —
/// each an ordinary directed frame riding the unicast → circuit → gossip
/// cascade, so the RPC plane is size-transparent like chat and task legs. RPC
/// frames are plumbing (never logged for anti-entropy); the shards reassemble
/// only in the receiver's dedicated store, and a big group is served from the
/// sender-side [`ShardCache`](agent_habilis_mesh::daemon::reassembly::ShardCache)
/// through the `shard/repair` RPC.
///
/// # Errors
/// Propagates a serialize/deliver failure and refuses a body past the input
/// ceiling or the receiver's per-group reassembly budget.
pub(crate) async fn send_directed_rpc(
    mesh: &MeshId,
    author: &Nickname,
    kind: MessageKind,
    body: MessageBody,
    state: &mut EventLoopState,
    sender: &MeshSender,
) -> anyhow::Result<()> {
    let signer = state.identity.clone();
    let single = Message::new_frame(mesh, author, kind.clone(), body.clone()).signed(&signer);
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        agent_habilis_mesh::logging::messages::log_out(&single);
        let bytes = Bytes::from(single.serialize()?);
        return agent_habilis_mesh::unicast::deliver(&single, bytes, state, sender).await;
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
    if !state.meshed && state.pending_outbound.remaining() < chunks.len() {
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
        agent_habilis_mesh::logging::messages::log_out(&msg);
        let bytes = Bytes::from(msg.serialize()?);
        if total > LOGGED_SHARD_GROUP_MAX_TOTAL {
            cache_frames.push(bytes.clone());
        }
        agent_habilis_mesh::unicast::deliver(&msg, bytes, state, sender).await?;
    }
    if !cache_frames.is_empty() {
        state.shard_cache.insert(group, cache_frames);
    }
    Ok(())
}

/// Arm a fresh ping round carrying the responder; the deadline-driven
/// `finalize_ping_round` delivers the RTT rows through it. Mirrors the IPC
/// `Ping` handler, which leaves `resp` unset and emits the `ping_report`
/// event instead.
async fn session_ping(
    mesh: &MeshId,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &MeshSender,
    resp: tokio::sync::oneshot::Sender<Vec<agent_habilis_mesh::gossip::event::PingRtt>>,
) -> bool {
    let now = tokio::time::Instant::now();
    state.ping_round = Some(Box::new(agent_habilis_mesh::daemon::state::PingRound {
        t1: now,
        deadline: now
            + std::time::Duration::from_secs(agent_habilis_mesh::util::tuning::ping_window_secs()),
        pongs: std::collections::HashMap::new(),
        resp: Some(resp),
    }));
    agent_habilis_mesh::gossip::broadcast_msg(
        sender,
        &Message::new_ping(mesh, author).signed(&state.identity),
    )
    .await;
    true
}

/// Handle one typed in-process [`SessionRequest`] (embed / MCP). `Send`
/// broadcasts via the shared helper and echoes the canonical [`Message`]
/// back on the oneshot; `Poll` returns the join-horizon-filtered buffer.
/// Returns `true` if anything was broadcast so the caller can refresh
/// `last_sent_at` (mirrors `handle_ipc_command`).
#[expect(
    clippy::too_many_lines,
    reason = "a dispatch match with one arm per SessionRequest variant (mirrors handle_ipc_command)"
)]
pub(crate) async fn handle_session_request(
    req: SessionRequest,
    mesh: &MeshId,
    author: &Nickname,
    state: &mut EventLoopState,
    app: &mut A2aApp,
    sender: &MeshSender,
    output: &output::Output,
) -> bool {
    match req {
        SessionRequest::Send { body, resp } => {
            let outcome = broadcast_message(mesh, author, body, state, sender, output)
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
            app.surfaced.poll_or_register(
                after,
                long,
                tokio::time::Instant::now(),
                crate::a2a::surfaced::PollResponder::Typed(resp),
            );
            false
        }
        SessionRequest::TaskStatus {
            task_id,
            state: task_state,
            note,
            resp,
        } => {
            let outcome = emit_task_status(
                mesh,
                author,
                &task_id,
                task_state,
                note.as_deref(),
                state,
                app,
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
                mesh, author, &task_id, &text, file, state, app, sender, output,
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
            let outcome = agent_habilis_mesh::gossip::broadcast_state_merge(
                mesh,
                author,
                merge,
                state,
                sender,
                output,
                Channel::State,
                true,
            )
            .await;
            let sent = outcome.is_ok();
            let _ = resp.send(outcome);
            sent
        }
        SessionRequest::StateGet { resp } => {
            let _ = resp.send(state.state_doc.to_json());
            false
        }
        SessionRequest::MetaMerge { merge, resp } => {
            let outcome = agent_habilis_mesh::gossip::broadcast_state_merge(
                mesh,
                author,
                merge,
                state,
                sender,
                output,
                Channel::Meta,
                true,
            )
            .await;
            let sent = outcome.is_ok();
            let _ = resp.send(outcome);
            sent
        }
        SessionRequest::MetaGet { resp } => {
            let _ = resp.send(state.meta_doc.to_json());
            false
        }
        SessionRequest::Ping { resp } => session_ping(mesh, author, state, sender, resp).await,
        SessionRequest::A2aCall {
            peer,
            method,
            params,
            timeout,
            resp,
        } => {
            broadcast_a2a_call(
                mesh,
                author,
                peer,
                &method,
                params,
                timeout,
                crate::a2a::app::A2aResponder::Typed(resp),
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
        // Index-size snapshot (adversarial only) for the leak regression test.
        #[cfg(feature = "adversarial")]
        SessionRequest::IndexStats { resp } => {
            let _ = resp.send((
                state.by_hash.len(),
                state.dag_heads.len(),
                state.author_seqs.len(),
            ));
            false
        }
        // Simulated stream end (adversarial only): indistinguishable from
        // the real `None` arm as far as the event loop is concerned.
        #[cfg(feature = "adversarial")]
        SessionRequest::SeverGossip => {
            state.gossip_open = false;
            tracing::warn!("gossip stream severed (adversarial); heal arm will resubscribe");
            false
        }
        // Inject a synthetic link-state vector (adversarial/testkit only) so a
        // circuit route can converge — a live mesh never converges usable
        // circuit edges for the transport matrix / linear-circuit tests.
        #[cfg(feature = "adversarial")]
        SessionRequest::InjectLinkVector {
            origin,
            seq,
            seal_key,
            links,
        } => {
            state
                .link_state
                .ingest(agent_habilis_mesh::circuit::LinkVector::from_raw(
                    origin, seq, seal_key, links,
                ));
            false
        }
        // Reassembly accounting snapshot (adversarial only) for the
        // shard-budget tripwires.
        #[cfg(feature = "adversarial")]
        SessionRequest::ReassemblyStats { resp } => {
            let _ = resp.send(state.reassembly.stats());
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
