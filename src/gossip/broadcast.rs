//! Gossip **outbound/send plane**: building, signing, logging and
//! broadcasting messages; the unmeshed-join outbound buffer; presence /
//! `PeerInfo` announcements; and the interactive `/reply` stdin path.
//! [`broadcast_message`] is the single source of truth for the send
//! path — the IPC `Msg` command, the typed in-process `SessionRequest`,
//! and stdin all funnel through it so they cannot drift. Inbound dispatch
//! lives in [`super::recv`].

use std::time::Instant;

use bytes::Bytes;
use iroh::Endpoint;
use iroh_gossip::api::GossipSender;

use crate::daemon::SessionRequest;
use crate::daemon::state::EventLoopState;
use crate::output;
use crate::protocol::identity::Identity;
use crate::protocol::{
    Channel, Message, MessageBody, MessageId, MessageKind, Nickname, Part, PartGroup, SwarmId,
    TaskId, TaskPhase,
};
use crate::util::consts::{MAX_MESSAGE_PARTS, MAX_MESSAGE_SIZE};

/// Fire-and-forget gossip broadcast. Serialize errors are swallowed:
/// this helper is for presence / `PeerInfo` announcements where a
/// failed serialize must not block the daemon. A failed *broadcast* is
/// logged — it means the gossip actor refused the send (the wedge the
/// roster-collapse soak hit silently), not a routine empty room.
pub(crate) async fn broadcast_msg(sender: &GossipSender, msg: &Message) {
    crate::logging::messages::log_out(msg);
    if let Ok(bytes) = msg.serialize()
        && let Err(error) = sender.broadcast(Bytes::from(bytes)).await
    {
        tracing::warn!(
            target: "agent_habilis_swarm::gossip",
            %error,
            "presence/plumbing broadcast failed"
        );
    }
}

/// Author + broadcast a durable `State` event, retaining it locally first.
/// Gossip never echoes to self, so the author must hold what it authored or the
/// state anti-entropy path could never serve it. The low-level primitive skips
/// the per-author `Msg` hash chain — state lives in its own un-pruned log.
/// When unmeshed the bytes are buffered for flush-on-connect;
/// even if that buffer is full the event is safe in the local state log, so
/// anti-entropy backfills peers once meshed.
///
/// # Errors
/// Serialization or broadcast failure.
#[expect(
    clippy::too_many_arguments,
    reason = "the channel-parameterized state write; every argument is load-bearing"
)]
pub(crate) async fn broadcast_state(
    swarm: &SwarmId,
    author: &Nickname,
    body: MessageBody,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
    channel: Channel,
    // The pre-insert document, when the caller already derived it. `None` ⇒
    // derive it here. Avoids a second full fold of the un-pruned state log.
    before: Option<serde_json::Value>,
) -> anyhow::Result<()> {
    let signed = Message::new_channel_event(swarm, author, body, channel).signed(&state.identity);
    // Serialize **before** the local insert: an oversize body fails here and the
    // event never enters the log, so the author can't hold a patch it can never
    // gossip (anti-entropy can't resend an un-serializable event either) — that
    // would diverge permanently. A failed *broadcast* below still inserts and is
    // recoverable via anti-entropy; only a failed *serialize* is blocked.
    let bytes = signed.serialize()?;
    crate::logging::messages::log_out(&signed);
    let log = match channel {
        Channel::State => &mut state.state_log,
        Channel::Meta => &mut state.meta_log,
    };
    let before = before.unwrap_or_else(|| crate::daemon::state_doc::derive_document(log));
    log.insert(signed.clone());
    let after = crate::daemon::state_doc::derive_document(log);
    // Surface our own change (is_self) when the document actually changed — a
    // no-op patch (and non-patch substrate state) surfaces nothing.
    if after != before {
        output.state_changed(channel, &signed, &after, true);
    }
    if state.meshed {
        sender
            .broadcast(Bytes::from(bytes))
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
    } else {
        let _ = state.pending_outbound.push(Bytes::from(bytes));
    }
    Ok(())
}

/// The single shared-state write helper, shared by the IPC `state_merge` command
/// and the embed `StateMerge` request. An RFC 7386 merge always applies (any JSON
/// value is valid), so this just composes the body and gossips via
/// [`broadcast_state`]. No size check here — `Message::serialize` is the single
/// size gate, inside `broadcast_state`.
///
/// # Errors
/// Propagates a `broadcast_state` failure (oversize body / broadcast refusal).
pub(crate) async fn broadcast_state_merge(
    swarm: &SwarmId,
    author: &Nickname,
    merge: serde_json::Value,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
    channel: Channel,
) -> anyhow::Result<()> {
    let body = crate::daemon::state_doc::merge_body(merge)?;
    broadcast_state(swarm, author, body, state, sender, output, channel, None).await
}

/// Broadcast a `PeerInfo` carrying our endpoint address so peers can
/// dial us directly. Unlike `joined`, `PeerInfo` never enters the
/// message log (`handle_peer_info` returns before the log push), so
/// re-sending it is invisible to `poll`/`fetch_messages` consumers —
/// safe to repeat on every new neighbor.
pub(super) async fn broadcast_peer_info(
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    identity: &Identity,
    endpoint: &Endpoint,
) {
    let our_addr = endpoint.addr();
    let addr_data = serde_json::to_string(&crate::protocol::peer_addr::endpoint_addr_to_json(
        &our_addr,
    ))
    .expect("endpoint_addr_to_json produces a Value that always serializes");
    let addr_body =
        MessageBody::new(addr_data).expect("endpoint address JSON has no control characters");
    broadcast_msg(
        sender,
        &Message::new_peer_info(swarm, author, addr_body).signed(identity),
    )
    .await;
}

/// Announce our arrival: `joined` presence followed by `PeerInfo`.
/// Called once at the top of the event loop; the caller bumps
/// `last_sent_at` afterwards.
pub(super) async fn announce_arrival(
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    identity: &Identity,
    endpoint: &Endpoint,
) {
    broadcast_msg(sender, &Message::new_joined(swarm, author).signed(identity)).await;
    broadcast_peer_info(sender, swarm, author, identity, endpoint).await;
}

/// Process one line of interactive stdin: parse a `/reply <nick> ...`
/// command or treat the line as a plain broadcast, validate the
/// nickname/body, then delegate to `broadcast_message` so the send
/// (and its oversize/serialize error handling) is identical to the
/// IPC and embed paths.
pub(crate) async fn handle_stdin_line(
    text: &str,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    state: &mut EventLoopState,
    out: &output::Output,
) {
    out.clear_input_line();
    if text.is_empty() {
        return;
    }
    let (body, reply) = if let Some((nick, raw_body)) = parse_reply_command(text) {
        let body = match MessageBody::new(raw_body) {
            Ok(body) => body,
            Err(error) => {
                out.report_error(&error);
                return;
            }
        };
        let Ok(target) = Nickname::new(nick) else {
            out.error(&format!("invalid nickname '{nick}'"));
            return;
        };
        (body, Some(target))
    } else {
        let body = match MessageBody::new(text) {
            Ok(body) => body,
            Err(error) => {
                out.report_error(&error);
                return;
            }
        };
        (body, None)
    };
    match broadcast_message(swarm, author, body, reply, state, sender, out).await {
        Ok(_) => state.last_sent_at = Instant::now(),
        Err(error) => out.report_error(&error),
    }
}

/// Retain a just-built outbound message in the local log (pruning the
/// fork/DAG indexes on any eviction) and write the dev log. The operator
/// echo is the caller's responsibility — it differs by kind
/// (`print_message_ex` for `Msg`, `print_task` for `Task`).
/// Shared by [`commit_outbound_part`] (after chain stamping) and
/// [`echo_and_retain_task`] (no chain stamping).
fn retain_outbound(state: &mut EventLoopState, msg: &Message) {
    if let Some(evicted) = state.message_log.push(msg.clone()) {
        let evicted_hash = evicted.content_hash_hex();
        state.forget_hash(&evicted_hash);
        if let Some(seq) = evicted.seq {
            state.forget_msg_seq(&evicted.pubkey, seq, &evicted_hash);
        }
    }
    crate::logging::messages::log_out(msg);
}

/// Echo a just-built outbound task leg to the operator and, for **content**
/// legs, retain it in the message log for anti-entropy. The `Progress` phase
/// is liveness plumbing — echoed (so the sender's own widget updates) but
/// never retained, mirroring its receive-side handling.
fn echo_and_retain_task(state: &mut EventLoopState, msg: &Message, out: &output::Output) {
    out.print_task(msg, true);
    retain_task(state, msg);
}

/// Retain a just-built outbound task leg for anti-entropy **without**
/// echoing it — the raw parts of a split leg retain silently; the reassembled
/// logical leg is echoed once. Content legs retain; the `Progress` beat doesn't.
fn retain_task(state: &mut EventLoopState, msg: &Message) {
    let retain = matches!(
        &msg.kind,
        MessageKind::Task { phase, .. } if crate::protocol::message::is_content_phase(*phase)
    );
    if retain {
        retain_outbound(state, msg);
    }
}

/// Commit a just-built outbound `Msg` into local state: advance the per-author
/// hash chain (`seq`/`prev`) and the DAG tips, retain it in the message log
/// (pruning the fork/DAG indexes on eviction), write the dev log, and — when
/// `echo` — print the operator line. The raw parts of a split body commit
/// silently (`echo == false`); only the reassembled logical message is echoed.
/// Shared by the meshed and queued send paths so the chain bookkeeping can't
/// drift between them.
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

/// Headroom subtracted from a part's body budget so the first part's measured
/// envelope still covers later parts, whose `seq` may have grown a digit or two.
const PART_BUDGET_MARGIN: usize = 16;

/// JSON-escaped byte length of one char inside a `"…"` string. `serde_json` keeps
/// non-ASCII as UTF-8; only the quote, backslash, and `\n`/`\t`/`\r` expand.
/// Used to split a body so each part's *serialized* size fits the wire cap.
fn escaped_char_len(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\n' | '\t' | '\r' => 2,
        ch if (ch as u32) < 0x20 => 6, // other controls → \u00XX (rejected by MessageBody)
        ch => ch.len_utf8(),
    }
}

/// The escaped-body budget per part: the wire cap minus the serialized envelope
/// of an empty-body part (built with worst-case header digits), minus margin.
fn part_body_budget(empty_part: &Message) -> usize {
    MAX_MESSAGE_SIZE.saturating_sub(empty_part.wire_len() + PART_BUDGET_MARGIN)
}

/// Split `body` into the fewest UTF-8-safe chunks whose JSON-escaped length each
/// fits `budget`. `None` if it would need more than [`MAX_MESSAGE_PARTS`] parts
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
        if chunks.len() > MAX_MESSAGE_PARTS {
            return None;
        }
    }
    Some(chunks)
}

/// Build, chain-stamp, part-tag and sign one outbound `Msg`/reply (without
/// serializing — the caller measures or serializes).
#[expect(
    clippy::too_many_arguments,
    reason = "a chained, part-tagged Msg needs the kind (reply), body, chain (seq/prev/parents), part header and signer; the daemon stamps these inline to interleave chain stamping with the multipart split"
)]
fn build_msg(
    swarm: &SwarmId,
    author: &Nickname,
    reply: Option<&Nickname>,
    body: MessageBody,
    seq: u64,
    prev: Option<String>,
    parents: Vec<String>,
    part: Option<Part>,
    signer: &Identity,
) -> Message {
    match reply {
        None => Message::new_message(swarm, author, body),
        Some(target) => Message::new_reply(swarm, author, target.clone(), body),
    }
    .with_chain(seq, prev)
    .with_parents(parents)
    .with_part(part)
    .signed(signer)
}

/// Broadcast (or buffer, while unmeshed) one fully-built `Msg` and commit it to
/// the per-author chain + log. `echo` gates the operator print so the raw parts
/// of a split body commit silently. Errors if the unmeshed buffer is full.
async fn send_msg_part(
    state: &mut EventLoopState,
    sender: &GossipSender,
    out: &output::Output,
    msg: &Message,
    bytes: Bytes,
    echo: bool,
) -> anyhow::Result<()> {
    if state.meshed {
        commit_outbound_part(state, msg, out, echo);
        sender
            .broadcast(bytes)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
    } else if state.pending_outbound.push(bytes) {
        commit_outbound_part(state, msg, out, echo);
    } else {
        tracing::warn!("pending outbound buffer full; outbound message dropped");
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; message dropped"
        ));
    }
    Ok(())
}

/// The reassembled logical `Msg` to echo + return after a multipart send. Its
/// `id` is the part `group`, so the sender and every receiver name the
/// reassembled body identically. Unsigned / unchained — a local view, not a wire
/// message (the parts carry the wire bytes and the chain entries).
fn synthesize_logical_msg(
    swarm: &SwarmId,
    author: &Nickname,
    reply: Option<&Nickname>,
    body: MessageBody,
    group: &PartGroup,
) -> Message {
    let mut msg = match reply {
        None => Message::new_message(swarm, author, body),
        Some(target) => Message::new_reply(swarm, author, target.clone(), body),
    };
    msg.id = MessageId::new(group.as_str()).expect("a part group is a valid message id");
    msg
}

/// Build, sign, log and gossip-broadcast one outbound message. The
/// single source of truth for the send path: the CLI socket's IPC `Msg`
/// command and the typed in-process `SessionRequest::Send` both funnel
/// through here so they cannot drift. A body too large for one message is
/// transparently split into `part`-tagged messages the receiver reassembles;
/// the returned [`Message`] is the whole logical body either way.
///
/// # Errors
/// Propagates a [`Message::serialize`] failure and a gossip broadcast error,
/// errors if the unmeshed pending-outbound buffer is full, and refuses a body
/// that would need more than [`MAX_MESSAGE_PARTS`] parts.
pub(crate) async fn broadcast_message(
    swarm: &SwarmId,
    author: &Nickname,
    body: MessageBody,
    reply: Option<Nickname>,
    state: &mut EventLoopState,
    sender: &GossipSender,
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
    let signer = state.identity.clone();
    // Fast path: the whole body in one message. If it fits the wire cap, send it
    // as today — no `part` header, so an ordinary message's wire form is
    // unchanged.
    let single = build_msg(
        swarm,
        author,
        reply.as_ref(),
        body.clone(),
        state.self_seq,
        state.self_prev.clone(),
        state.dag_parents(),
        None,
        &signer,
    );
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        let bytes = Bytes::from(single.serialize()?);
        let id = single.id.clone();
        send_msg_part(state, sender, out, &single, bytes, true).await?;
        return Ok((id, single));
    }
    // Too big: split the body across part-tagged messages. Each part is an
    // ordinary chained `Msg`, retained for anti-entropy; only the reassembled
    // body is echoed to the operator and returned.
    let group = PartGroup::random();
    let max_parts = u32::try_from(MAX_MESSAGE_PARTS).unwrap_or(u32::MAX);
    // Size the probe against the *largest* part's envelope, not the first's. Each
    // later part chains off the prior, so even from genesis (empty `prev`/`parents`)
    // every part after the first carries a 64-char `prev` and one parent hash.
    // Worst-case both: a full-length `prev` and as many parent hashes as the first
    // part could hold (its current tips, never fewer than the one-hash link later
    // parts carry). A 64-char zero string stands in for any content hash.
    let hash_stub = "0".repeat(64);
    let probe_parents = vec![hash_stub.clone(); state.dag_parents().len().max(1)];
    let probe = build_msg(
        swarm,
        author,
        reply.as_ref(),
        MessageBody::new(String::new()).expect("empty body is valid"),
        state.self_seq,
        Some(hash_stub),
        probe_parents,
        Some(Part {
            group: group.clone(),
            idx: max_parts - 1,
            total: max_parts,
        }),
        &signer,
    );
    let budget = part_body_budget(&probe);
    let chunks = split_body(body.as_str(), budget).ok_or_else(|| {
        anyhow::anyhow!(
            "message too large: a {}-byte body needs more than {MAX_MESSAGE_PARTS} parts",
            body.as_str().len()
        )
    })?;
    let total = u32::try_from(chunks.len()).expect("chunk count is bounded by MAX_MESSAGE_PARTS");
    // Atomic admission while unmeshed: all parts or none, so a half-buffered body
    // (which could never reassemble) never reaches peers.
    if !state.meshed && state.pending_outbound.remaining() < chunks.len() {
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; multipart message dropped"
        ));
    }
    for (idx, chunk) in chunks.iter().enumerate() {
        let part = Part {
            group: group.clone(),
            idx: u32::try_from(idx).expect("idx is bounded by MAX_MESSAGE_PARTS"),
            total,
        };
        let chunk_body = MessageBody::new(*chunk).expect("a substring of a valid body is valid");
        let msg = build_msg(
            swarm,
            author,
            reply.as_ref(),
            chunk_body,
            state.self_seq,
            state.self_prev.clone(),
            state.dag_parents(),
            Some(part),
            &signer,
        );
        let bytes = Bytes::from(msg.serialize()?);
        send_msg_part(state, sender, out, &msg, bytes, false).await?;
    }
    let logical = synthesize_logical_msg(swarm, author, reply.as_ref(), body, &group);
    out.print_message_ex(&logical, true);
    Ok((logical.id.clone(), logical))
}

/// One outbound task leg's payload (addressee + correlation id +
/// phase + body), bundled so [`broadcast_task`] stays within the argument
/// budget. The IPC `task` command and the typed `SessionRequest::Task` both
/// build it from their carried fields.
pub(crate) struct TaskLeg {
    pub to: Nickname,
    pub task_id: TaskId,
    pub phase: TaskPhase,
    pub body: MessageBody,
}

/// Build, sign, log and gossip-broadcast one task leg. Sibling of
/// [`broadcast_message`] for the typed `Task` kind: **content** legs take the
/// meshed/unmeshed retain paths, but the `Progress` phase is liveness plumbing —
/// never retained. No hash-chain or DAG stamping (task legs are presence-like —
/// see [`MessageKind::Task`]). Always echoes the sender's own leg through
/// `print_task` (a `task`/`task_progress` event with `self:true`), the same
/// way an outbound `msg` echoes. Showing the leg to its *addressee* is the
/// receiver-side job of [`lifecycle::handle_task`](crate::lifecycle::handle_task).
///
/// This is where `Offer` addressee validation lives: an `Offer` leg must name
/// a current participant. The CLI, MCP, and embed callers all reach this
/// function, so validating here covers every path. Later phases skip the check
/// so a brief peer flap mid-task can't wedge the conversation.
///
/// # Errors
/// Returns an `unknown participant` error for an `Offer` to a non-participant;
/// propagates [`Message::serialize`] failure (oversized brief) and a gossip
/// broadcast error.
pub(crate) async fn broadcast_task(
    swarm: &SwarmId,
    author: &Nickname,
    leg: TaskLeg,
    state: &mut EventLoopState,
    sender: &GossipSender,
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
    let TaskLeg {
        to,
        task_id,
        phase,
        body,
    } = leg;
    if matches!(phase, TaskPhase::Offer) && !state.participants.contains(to.as_str()) {
        return Err(anyhow::anyhow!("unknown participant '{to}'"));
    }
    let signer = state.identity.clone();
    // Fast path: the whole leg in one message.
    let single = build_task(
        swarm,
        author,
        &to,
        &task_id,
        phase,
        body.clone(),
        None,
        &signer,
    );
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        let bytes = Bytes::from(single.serialize()?);
        let id = single.id.clone();
        send_task_leg(state, sender, out, &single, bytes, true).await?;
        ingest_own_leg(state, &single, out);
        return Ok((id, single));
    }
    // Only content legs are ever large enough to split; the `Progress` beat is a
    // tiny liveness widget and is never retained/reassembled.
    if !crate::protocol::message::is_content_phase(phase) {
        return Err(anyhow::anyhow!("task {phase} leg too large to send"));
    }
    let group = PartGroup::random();
    let max_parts = u32::try_from(MAX_MESSAGE_PARTS).unwrap_or(u32::MAX);
    let probe = build_task(
        swarm,
        author,
        &to,
        &task_id,
        phase,
        MessageBody::new(String::new()).expect("empty body is valid"),
        Some(Part {
            group: group.clone(),
            idx: max_parts - 1,
            total: max_parts,
        }),
        &signer,
    );
    let budget = part_body_budget(&probe);
    let chunks = split_body(body.as_str(), budget).ok_or_else(|| {
        anyhow::anyhow!(
            "task leg too large: a {}-byte body needs more than {MAX_MESSAGE_PARTS} parts",
            body.as_str().len()
        )
    })?;
    let total = u32::try_from(chunks.len()).expect("chunk count is bounded by MAX_MESSAGE_PARTS");
    if !state.meshed && state.pending_outbound.remaining() < chunks.len() {
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; multipart task leg dropped"
        ));
    }
    for (idx, chunk) in chunks.iter().enumerate() {
        let part = Part {
            group: group.clone(),
            idx: u32::try_from(idx).expect("idx is bounded by MAX_MESSAGE_PARTS"),
            total,
        };
        let chunk_body = MessageBody::new(*chunk).expect("a substring of a valid body is valid");
        let msg = build_task(
            swarm,
            author,
            &to,
            &task_id,
            phase,
            chunk_body,
            Some(part),
            &signer,
        );
        let bytes = Bytes::from(msg.serialize()?);
        send_task_leg(state, sender, out, &msg, bytes, false).await?;
    }
    // Echo + ingest the logical leg once (one content leg toward the cap).
    let mut logical = Message::new_task(swarm, author, to, task_id, phase, body);
    logical.id = MessageId::new(group.as_str()).expect("a part group is a valid message id");
    out.print_task(&logical, true);
    ingest_own_leg(state, &logical, out);
    Ok((logical.id.clone(), logical))
}

/// Build, part-tag and sign one outbound task leg (no serialize).
#[expect(
    clippy::too_many_arguments,
    reason = "a task leg carries to/task_id/phase/body plus the part header and signer; bundling them buys nothing over the existing TaskLeg"
)]
fn build_task(
    swarm: &SwarmId,
    author: &Nickname,
    to: &Nickname,
    task_id: &TaskId,
    phase: TaskPhase,
    body: MessageBody,
    part: Option<Part>,
    signer: &Identity,
) -> Message {
    Message::new_task(swarm, author, to.clone(), task_id.clone(), phase, body)
        .with_part(part)
        .signed(signer)
}

/// Broadcast (or buffer, while unmeshed) one fully-built task leg, retaining
/// content legs for anti-entropy. `echo` gates the operator print so the raw
/// parts of a split leg commit silently. Errors if the unmeshed buffer is full.
async fn send_task_leg(
    state: &mut EventLoopState,
    sender: &GossipSender,
    out: &output::Output,
    msg: &Message,
    bytes: Bytes,
    echo: bool,
) -> anyhow::Result<()> {
    if state.meshed {
        // Meshed: retain locally, then hit the wire (a transient broadcast error
        // still leaves a content leg in our log for anti-entropy).
        retain_leg(state, msg, out, echo);
        sender
            .broadcast(bytes)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
    } else if state.pending_outbound.push(bytes) {
        retain_leg(state, msg, out, echo);
    } else {
        tracing::warn!("pending outbound buffer full; outbound message dropped");
        return Err(anyhow::anyhow!(
            "pending outbound buffer full; message dropped"
        ));
    }
    Ok(())
}

/// Retain a task leg locally, echoing the operator line only when `echo`
/// (false for the raw parts of a split leg). Content legs retain; `Progress`
/// doesn't — see [`retain_task`].
fn retain_leg(state: &mut EventLoopState, msg: &Message, out: &output::Output, echo: bool) {
    if echo {
        echo_and_retain_task(state, msg, out);
    } else {
        retain_task(state, msg);
    }
}

/// Advance our own coarse task state machine for a leg we just sent and warn
/// once if a content leg pushed the task past its whole-task cap.
fn ingest_own_leg(state: &mut EventLoopState, msg: &Message, out: &output::Output) {
    if crate::daemon::task::ingest(&mut state.tasks, msg, true, Instant::now()) {
        out.info(&format!(
            "task exceeded {} messages; wrap it up",
            crate::util::consts::TASK_CONTENT_CAP
        ));
        tracing::warn!("task content cap exceeded");
    }
}

/// Handle one typed in-process [`SessionRequest`] (embed / MCP). `Send`
/// broadcasts via the shared helper and echoes the canonical [`Message`]
/// back on the oneshot; `Poll` returns the join-horizon-filtered buffer.
/// Returns `true` if anything was broadcast so the caller can refresh
/// `last_sent_at` (mirrors `handle_ipc_command`).
pub(crate) async fn handle_session_request(
    req: SessionRequest,
    swarm: &SwarmId,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
) -> bool {
    match req {
        SessionRequest::Send { body, reply, resp } => {
            let outcome = broadcast_message(swarm, author, body, reply, state, sender, output)
                .await
                .map(|(_id, msg)| msg);
            let sent_ok = outcome.is_ok();
            let _ = resp.send(outcome);
            sent_ok
        }
        SessionRequest::Poll {
            after,
            wait_ms,
            resp,
        } => {
            // Same policy as the CLI/IPC `Poll` arm: respond now if events are
            // buffered, else (with `wait_ms`) park a typed waiter the loop
            // fulfills/expires. A parked waiter broadcasts nothing → `false`.
            state.poll_or_register(
                after,
                wait_ms,
                tokio::time::Instant::now(),
                crate::daemon::state::PollResponder::Typed(resp),
            );
            false
        }
        SessionRequest::Task {
            to,
            task_id,
            phase,
            body,
            resp,
        } => {
            let leg = TaskLeg {
                to,
                task_id,
                phase,
                body,
            };
            let outcome = broadcast_task(swarm, author, leg, state, sender, output)
                .await
                .map(|(_id, msg)| msg);
            let sent_ok = outcome.is_ok();
            let _ = resp.send(outcome);
            sent_ok
        }
        SessionRequest::Peers { resp } => {
            let _ = resp.send(state.roster_snapshot());
            false
        }
        SessionRequest::StateMerge { merge, resp } => {
            let outcome =
                broadcast_state_merge(swarm, author, merge, state, sender, output, Channel::State)
                    .await;
            let sent = outcome.is_ok();
            let _ = resp.send(outcome);
            sent
        }
        SessionRequest::StateGet { resp } => {
            let _ = resp.send(crate::daemon::state_doc::derive_document(&state.state_log));
            false
        }
        SessionRequest::MetaMerge { merge, resp } => {
            let outcome =
                broadcast_state_merge(swarm, author, merge, state, sender, output, Channel::Meta)
                    .await;
            let sent = outcome.is_ok();
            let _ = resp.send(outcome);
            sent
        }
        SessionRequest::MetaGet { resp } => {
            let _ = resp.send(crate::daemon::state_doc::derive_document(&state.meta_log));
            false
        }
        SessionRequest::Ping { resp } => {
            // Arm a fresh round carrying the responder; the deadline-driven
            // `finalize_ping_round` delivers the RTT rows through it. Mirrors
            // the IPC `Ping` handler, which leaves `resp` unset and emits the
            // `ping_report` event instead.
            let now = tokio::time::Instant::now();
            state.ping_round = Some(Box::new(crate::daemon::state::PingRound {
                t1: now,
                deadline: now
                    + std::time::Duration::from_secs(crate::util::tuning::ping_window_secs()),
                pongs: std::collections::HashMap::new(),
                resp: Some(resp),
            }));
            broadcast_msg(
                sender,
                &Message::new_ping(swarm, author).signed(&state.identity),
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
    }
}

/// Parse `/reply <nickname> body` from interactive stdin input.
/// Returns `Some((nickname, body))` if the input matches, `None`
/// otherwise. Lives here because the interactive stdin handler
/// (`handle_stdin_line`) is its only consumer. `<`/`>` are reserved
/// (not valid in a nickname), so the bracket delimiters are unambiguous.
fn parse_reply_command(input: &str) -> Option<(&str, &str)> {
    let rest = input.strip_prefix("/reply ")?;
    let rest = rest.strip_prefix('<')?;
    let bracket_end = rest.find('>')?;
    let nickname = &rest[..bracket_end];
    let body = rest[bracket_end + 1..].trim();
    if nickname.is_empty() || body.is_empty() {
        return None;
    }
    Some((nickname, body))
}

#[cfg(test)]
mod parse_reply_tests {
    use super::parse_reply_command;

    #[test]
    fn parse_reply_valid() {
        assert_eq!(
            parse_reply_command("/reply <alice> hello world"),
            Some(("alice", "hello world"))
        );
    }

    #[test]
    fn parse_reply_trims_body() {
        assert_eq!(
            parse_reply_command("/reply <bob>   spaced out  "),
            Some(("bob", "spaced out"))
        );
    }

    #[test]
    fn parse_reply_not_a_reply() {
        assert_eq!(parse_reply_command("just a normal message"), None);
    }

    #[test]
    fn parse_reply_missing_brackets() {
        assert_eq!(parse_reply_command("/reply alice hello"), None);
    }

    #[test]
    fn parse_reply_empty_nickname() {
        assert_eq!(parse_reply_command("/reply <> hello"), None);
    }

    #[test]
    fn parse_reply_empty_body() {
        assert_eq!(parse_reply_command("/reply <alice>"), None);
        assert_eq!(parse_reply_command("/reply <alice>   "), None);
    }

    #[test]
    fn parse_reply_hyphenated_nickname() {
        assert_eq!(
            parse_reply_command("/reply <bright-fern> thanks!"),
            Some(("bright-fern", "thanks!"))
        );
    }
}

#[cfg(test)]
mod split_body_tests {
    use super::{escaped_char_len, split_body};
    use crate::util::consts::MAX_MESSAGE_PARTS;

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
        let chunks = split_body(&body, 16).expect("60 bytes at budget 16 fits in MAX parts");
        assert!(chunks.len() > 1, "the body must actually split");
        assert!(chunks.len() <= MAX_MESSAGE_PARTS);
        assert_eq!(chunks.concat(), body, "chunks reassemble to the original");
        for chunk in &chunks {
            let escaped: usize = chunk.chars().map(escaped_char_len).sum();
            assert!(escaped <= 16, "each chunk's escaped length fits the budget");
        }
    }

    #[test]
    fn splits_multibyte_on_char_boundaries() {
        let body = "héllo🌍".repeat(8); // 2-byte é and 4-byte emoji
        let chunks = split_body(&body, 8).expect("fits in MAX parts");
        assert_eq!(chunks.concat(), body, "no char is split mid-codepoint");
        assert!(chunks.iter().all(|chunk| !chunk.is_empty()));
    }

    #[test]
    fn refuses_a_body_needing_too_many_parts() {
        let body = "x".repeat(1000);
        // One byte per part would need 1000 parts, far over MAX_MESSAGE_PARTS.
        assert!(split_body(&body, 1).is_none());
    }
}
