//! Gossip **outbound/send plane**: building, signing, logging and
//! broadcasting messages; the unmeshed-join outbound buffer; presence /
//! `PeerInfo` announcements; and the interactive broadcast stdin path.
//! [`broadcast_message`] is the single source of truth for the send
//! path — the IPC `Msg` command, the typed in-process `SessionRequest`,
//! and stdin all funnel through it so they cannot drift. Inbound dispatch
//! lives in [`super::recv`].

use std::time::Instant;

use crate::transport::SwarmSender;
use bytes::Bytes;
use iroh::Endpoint;

use crate::daemon::SessionRequest;
use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::output;
use crate::protocol::identity::Identity;
use crate::protocol::{
    Channel, Message, MessageBody, MessageId, MessageKind, Nickname, Shard, ShardGroup, SwarmId,
};
use crate::util::consts::{
    LOGGED_SHARD_GROUP_MAX_TOTAL, MAX_LOGICAL_BODY_BYTES, MAX_MESSAGE_SIZE, MAX_SHARD_TOTAL,
    REASSEMBLY_GROUP_MAX_BYTES,
};

/// Fire-and-forget gossip broadcast. Serialize errors are swallowed:
/// this helper is for presence / `PeerInfo` announcements where a
/// failed serialize must not block the daemon. A failed *broadcast* is
/// logged — it means the gossip actor refused the send (the wedge the
/// roster-collapse soak hit silently), not a routine empty room.
pub(crate) async fn broadcast_msg(sender: &SwarmSender, msg: &Message) {
    crate::logging::messages::log_out(msg);
    if let Ok(bytes) = msg.serialize()
        && let Err(error) = sender.broadcast(Bytes::from(bytes)).await
    {
        tracing::warn!(
            target: "agent_gossip::gossip",
            %error,
            "presence/plumbing broadcast failed"
        );
    }
}

/// The single shared-state write helper, shared by the IPC `state_merge` command
/// and the embed `StateMerge` request. Translates the RFC 7386 merge into one
/// automerge change, gossips it inside a signed frame, and retains it locally so
/// anti-entropy can serve it (gossip never echoes to self).
///
/// The change is built on a fork and the frame size-gated **before** the change
/// touches the live doc, so an oversize merge never lands in a doc it could not
/// be gossiped for. Applying then runs through [`SwarmDoc::ingest`]'s gate, so a
/// local write that would forge another peer's card is refused, not silently
/// diverged.
///
/// `surface` controls whether the local write is reported to the operator/agent
/// as a `state`/`meta` event (`💬️ you changed …`). Agent-driven merges pass
/// `true`; the daemon's own automatic card publish passes `false` — that write is
/// internal plumbing, not something the agent did, so it must not appear as a
/// "you changed shared state" event (nor race into a `fetch_messages` long-poll).
///
/// # Errors
/// Unrepresentable merge, oversize frame, a rejected foreign-card write, or a
/// broadcast refusal.
pub(crate) async fn broadcast_state_merge(
    ctx: &HandlerCtx<'_>,
    merge: serde_json::Value,
    state: &mut EventLoopState,
    channel: Channel,
    surface: bool,
) -> anyhow::Result<()> {
    use crate::daemon::doc::Ingested;

    // 1. Build the change on a fork (no live mutation yet); a no-op merge is a
    //    silent success.
    let built = match channel {
        Channel::State => state.state_doc.build_change(&merge, ctx.author)?,
        Channel::Meta => state.meta_doc.build_change(&merge, ctx.author)?,
    };
    let Some(change_bytes) = built else {
        return Ok(());
    };

    // 2. Compose + sign + size-gate before the change touches the live doc.
    //    Carry the input merge as the surfaced delta only for agent-visible
    //    writes; the internal card publish stays lean (no delta on the wire).
    //    On a passworded swarm the wire body is sealed under the channel key;
    //    `plain_body` keeps the plaintext for our own surfacing below.
    let (wire_body, plain_body) = match channel {
        Channel::State => state
            .state_doc
            .compose_wire_body(&change_bytes, surface.then_some(&merge))?,
        Channel::Meta => state
            .meta_doc
            .compose_wire_body(&change_bytes, surface.then_some(&merge))?,
    };
    let signed = Message::new_channel_event(ctx.swarm, ctx.author, wire_body, channel)
        .signed(&state.identity);
    let bytes = signed.serialize()?;
    crate::logging::messages::log_out(&signed);

    // 3. Apply the signed frame to the live doc through the authorization gate.
    //    Ingest retains the frame as the re-serve store (replacing `StateLog`),
    //    so anti-entropy can forward it with its original signature.
    let ingested = match channel {
        Channel::State => state.state_doc.ingest(&signed),
        Channel::Meta => state.meta_doc.ingest(&signed),
    };
    let after = match ingested {
        Ingested::Applied { doc, .. } => doc,
        Ingested::Duplicate => return Ok(()),
        Ingested::Rejected => {
            anyhow::bail!("write rejected: a member may only write its own /peers/<nick>/card")
        }
        Ingested::Buffered => unreachable!("a locally-built change's deps are always present"),
        Ingested::Ignored => unreachable!("a locally-built change body always decodes"),
    };

    // 4. Surface our own change, and gossip it (or buffer when unmeshed — the
    //    change is safe in the local doc for heads-based anti-entropy).
    if surface {
        // Surface our own change from a plaintext-bodied view so the `m` delta
        // renders even when the wire body we signed is sealed.
        let mut surfaced = signed.clone();
        surfaced.body = plain_body;
        ctx.output.state_changed(channel, &surfaced, &after, true);
    }
    if state.meshed {
        ctx.sender
            .broadcast(Bytes::from(bytes))
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
    } else {
        // Unmeshed: buffer for gossip until we mesh, and mirror to the spool
        // only when the buffer accepted the frame — so we never export a change
        // we've reported as un-buffered. (The change is already in the local
        // doc, so heads anti-entropy still reconciles it if the buffer is full.)
        let frame = Bytes::from(bytes);
        if state.pending_outbound.push(frame.clone()) {
            ctx.sender.spool(&frame);
        }
    }
    Ok(())
}

/// Broadcast a `PeerInfo` carrying our endpoint address so peers can
/// dial us directly. Unlike `joined`, `PeerInfo` never enters the
/// message log (`handle_peer_info` returns before the log push), so
/// re-sending it is invisible to `poll`/`fetch_messages` consumers —
/// safe to repeat on every new neighbor.
pub(super) async fn broadcast_peer_info(
    sender: &SwarmSender,
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
    sender: &SwarmSender,
    swarm: &SwarmId,
    author: &Nickname,
    identity: &Identity,
    endpoint: &Endpoint,
) {
    broadcast_msg(sender, &Message::new_joined(swarm, author).signed(identity)).await;
    broadcast_peer_info(sender, swarm, author, identity, endpoint).await;
}

/// Process one line of interactive stdin as a swarm broadcast (A2A is
/// point-to-point, so directed 1:1 is a task, not a chat line) — validate the
/// body, then delegate to `broadcast_message` so the send (and its
/// oversize/serialize error handling) is identical to the IPC and embed paths.
pub(crate) async fn handle_stdin_line(
    text: &str,
    sender: &SwarmSender,
    swarm: &SwarmId,
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
    match broadcast_message(swarm, author, body, state, sender, out).await {
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
    // Big-group shards stay out of the sender's log exactly as they stay out
    // of every receiver's (`shard_fits_log`): one huge body must not evict
    // our anti-entropy history, and logging what receivers refuse to retain
    // would re-flood those shards on every digest round. Big groups heal via
    // shard repair (`state.shard_cache`) instead.
    if crate::protocol::message::shard_fits_log(msg)
        && let Some(evicted) = state.message_log.push(msg.clone())
    {
        let evicted_hash = evicted.content_hash_hex();
        state.forget_hash(&evicted_hash);
        if let Some(seq) = evicted.seq {
            state.forget_msg_seq(&evicted.pubkey, seq, &evicted_hash);
        }
    }
    crate::logging::messages::log_out(msg);
}

/// Commit a just-built outbound `Msg` into local state: advance the per-author
/// hash chain (`seq`/`prev`) and the DAG tips, retain it in the message log
/// (pruning the fork/DAG indexes on eviction), write the dev log, and — when
/// `echo` — print the operator line. The raw shards of a split body commit
/// silently (`echo == false`); only the reassembled logical message is echoed.
/// Shared by the meshed and queued send paths so the chain bookkeeping can't
/// drift between them.
fn commit_outbound_part(
    state: &mut EventLoopState,
    msg: &Message,
    out: &output::Output,
    echo: bool,
) {
    // Only a chained frame advances the chain + DAG. Shards are unchained
    // (see `build_msg`): folding them in would grow `by_hash`/`dag_heads`
    // with entries only log eviction prunes — and big-group shards never
    // enter the log.
    if msg.seq.is_some() {
        let hash = msg.content_hash_hex();
        state.self_seq += 1;
        state.self_prev = Some(hash.clone());
        state.note_dag(hash, &msg.parents, msg.timestamp);
    }
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

/// Split `body` into the fewest UTF-8-safe chunks whose JSON-escaped length
/// each fits `budget`. `None` only when `budget` is zero — the shard *count*
/// is unbounded here; the callers gate on the byte ceilings instead
/// ([`gate_multipart`]).
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
    }
    Some(chunks)
}

/// Sender-side admission for a split body: enforce exactly what receivers
/// will refuse, so an oversize send fails loudly here instead of silently
/// dying on every peer. Returns the shard `total`.
fn gate_multipart(chunks: &[&str], what: &str) -> anyhow::Result<u32> {
    let total = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
    if total > MAX_SHARD_TOTAL {
        return Err(anyhow::anyhow!(
            "{what} too large: needs {total} shards (max {MAX_SHARD_TOTAL})"
        ));
    }
    // Mirror the reassembly store's per-group byte accounting.
    let buffered: usize = chunks
        .iter()
        .map(|chunk| crate::daemon::reassembly::slot_charge(chunk.len()))
        .sum();
    if buffered > REASSEMBLY_GROUP_MAX_BYTES {
        return Err(anyhow::anyhow!(
            "{what} too large: {buffered} buffered bytes exceed the receiver's \
             per-group budget ({REASSEMBLY_GROUP_MAX_BYTES}); use the blob channel for files"
        ));
    }
    Ok(total)
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
/// the A2A id there). `stamp: None` builds an **unchained** frame — the shape
/// every shard of a split body takes (like task-leg `a2a_msg` frames): shards
/// are transport slices, not chain entries, and stamping them would grow the
/// fork/DAG indexes with entries no log eviction ever prunes.
fn build_msg(
    swarm: &SwarmId,
    author: &Nickname,
    body: MessageBody,
    id: Option<MessageId>,
    stamp: Option<ChainStamp>,
    shard: Option<Shard>,
    signer: &Identity,
) -> Message {
    let mut msg = Message::new_a2a_msg(swarm, author, body);
    if let Some(id) = id {
        msg = msg.with_id(id);
    }
    if let Some(stamp) = stamp {
        msg = msg.with_chain(stamp.seq, stamp.prev).with_parents(stamp.parents);
    }
    msg.with_shard(shard).signed(signer)
}

/// Broadcast (or buffer, while unmeshed) one fully-built `Msg` and commit it to
/// the per-author chain + log. `echo` gates the operator print so the raw shards
/// of a split body commit silently. Errors if the unmeshed buffer is full.
async fn send_msg_part(
    state: &mut EventLoopState,
    sender: &SwarmSender,
    out: &output::Output,
    msg: &Message,
    bytes: Bytes,
    echo: bool,
) -> anyhow::Result<()> {
    if state.meshed {
        commit_outbound_part(state, msg, out, echo);
        // Single send decision: a directed message goes point-to-point over
        // unicast when the addressee is dialable, else gossip (see `unicast`).
        crate::unicast::deliver(msg, bytes, state, sender).await?;
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
    swarm: &SwarmId,
    author: &Nickname,
    body: MessageBody,
    group: &ShardGroup,
) -> Message {
    let mut msg = Message::new_a2a_msg(swarm, author, body);
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
/// past the local input ceiling ([`MAX_LOGICAL_BODY_BYTES`]) or the
/// receiver's per-group reassembly budget.
pub(crate) async fn broadcast_message(
    swarm: &SwarmId,
    author: &Nickname,
    text: MessageBody,
    state: &mut EventLoopState,
    sender: &SwarmSender,
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
    let signer = state.identity.clone();
    if text.as_str().len() > MAX_LOGICAL_BODY_BYTES {
        return Err(anyhow::anyhow!(
            "message too large: {} bytes exceeds the {MAX_LOGICAL_BODY_BYTES}-byte input \
             ceiling; use the blob channel for files",
            text.as_str().len()
        ));
    }
    let payload = crate::a2a::gossip::chat_message(swarm, text.as_str());
    let payload_id =
        MessageId::new(payload.message_id.as_str()).expect("an a2a message id is a valid frame id");
    let body = crate::a2a::gossip::payload_body(&payload)?;
    // On a passworded swarm, seal the chat body so a relay or a captured frame
    // can't read it. Everything downstream (dedup, the per-author chain, the
    // message log, anti-entropy) operates on the sealed frame; only the local
    // echo + the returned Message carry plaintext.
    let wire_body = match state.broadcast_key.as_deref() {
        Some(key) => crate::daemon::state_doc::encrypt_body(&body, key)?,
        None => body.clone(),
    };
    let encrypted = state.broadcast_key.is_some();
    // Fast path: the whole payload in one frame, its id the A2A messageId.
    let single = build_msg(
        swarm,
        author,
        wire_body.clone(),
        Some(payload_id.clone()),
        Some(ChainStamp {
            seq: state.self_seq,
            prev: state.self_prev.clone(),
            parents: state.dag_parents(),
        }),
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
    // ordinary signed (unchained) frame with its own minted id — small groups
    // retained for anti-entropy, big groups repair-served from the shard
    // cache; the *group* carries the A2A messageId, so the reassembled
    // logical frame — the only thing echoed and returned — is named by it.
    let group = ShardGroup::from_uuid_str(payload.message_id.as_str())
        .expect("an a2a message id is a valid shard group");
    // Shards are unchained (see `build_msg`), so the probe's envelope only
    // varies by the shard header — sized worst-case (max idx/total digits).
    let probe = build_msg(
        swarm,
        author,
        MessageBody::new(String::new()).expect("empty body is valid"),
        None,
        None,
        Some(Shard {
            group: group.clone(),
            idx: MAX_SHARD_TOTAL - 1,
            total: MAX_SHARD_TOTAL,
        }),
        &signer,
    );
    let budget = shard_body_budget(&probe);
    // Split the *wire* (sealed, when encrypted) body — shards carry chunks of
    // the ciphertext envelope that reassemble into it; the receiver decrypts the
    // reassembled body. The echoed/returned logical still carries plaintext.
    let chunks = split_body(wire_body.as_str(), budget)
        .ok_or_else(|| anyhow::anyhow!("shard body budget is zero; cannot split"))?;
    let total = gate_multipart(&chunks, "message")?;
    // Atomic admission while unmeshed: all shards or none, so a half-buffered body
    // (which could never reassemble) never reaches peers.
    if !state.meshed && state.pending_outbound.remaining() < chunks.len() {
        return Err(anyhow::anyhow!(
            "swarm still connecting: a {}-shard body exceeds the pre-mesh outbound buffer \
             ({} slots free); retry once a peer is connected",
            chunks.len(),
            state.pending_outbound.remaining()
        ));
    }
    let mut cache_frames = Vec::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        let shard = Shard {
            group: group.clone(),
            idx: u32::try_from(idx).expect("idx is bounded by the gate above"),
            total,
        };
        let chunk_body = MessageBody::new(*chunk).expect("a substring of a valid body is valid");
        let msg = build_msg(swarm, author, chunk_body, None, None, Some(shard), &signer);
        let bytes = Bytes::from(msg.serialize()?);
        if total > LOGGED_SHARD_GROUP_MAX_TOTAL {
            cache_frames.push(bytes.clone());
        }
        send_msg_part(state, sender, out, &msg, bytes, false).await?;
    }
    // A big group's shards skip every log (see `shard_fits_log`); the cache is
    // their re-serve source for peers' `shard/repair` requests.
    if !cache_frames.is_empty() {
        state.shard_cache.insert(group.clone(), cache_frames);
    }
    let logical = synthesize_logical_msg(swarm, author, body, &group);
    out.print_message_ex(&logical, true);
    Ok((logical.id.clone(), logical))
}

/// One outbound task leg's input (addressee + correlation id + verb +
/// body), bundled so [`broadcast_task`] stays within the argument budget.
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
async fn broadcast_task_frame(
    ctx: &HandlerCtx<'_>,
    kind: MessageKind,
    payload_body: MessageBody,
    content: bool,
    state: &mut EventLoopState,
) -> anyhow::Result<(MessageId, Message)> {
    let signer = state.identity.clone();
    // Fast path: the whole leg in one frame.
    let single = Message::new_frame(ctx.swarm, ctx.author, kind.clone(), payload_body.clone())
        .signed(&signer);
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        let bytes = Bytes::from(single.serialize()?);
        let id = single.id.clone();
        send_task_leg(state, ctx.sender, ctx.output, &single, bytes, true, content).await?;
        ingest_own_leg(state, &single, ctx.output);
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
    // `payload_body` is already sealed (base58, ~1.37x the input), so gate on
    // the sealed ceiling — the raw ceiling here would silently shrink the
    // documented 64 MiB limit for directed sends only.
    if payload_body.as_str().len() > crate::util::consts::MAX_SEALED_BODY_BYTES {
        return Err(anyhow::anyhow!(
            "task leg too large: {} sealed bytes exceeds the input ceiling \
             ({MAX_LOGICAL_BODY_BYTES} raw bytes); use the blob channel for files",
            payload_body.as_str().len()
        ));
    }
    let probe = Message::new_frame(
        ctx.swarm,
        ctx.author,
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
    let chunks = split_body(payload_body.as_str(), budget)
        .ok_or_else(|| anyhow::anyhow!("shard body budget is zero; cannot split"))?;
    let total = gate_multipart(&chunks, "task leg")?;
    if !state.meshed && state.pending_outbound.remaining() < chunks.len() {
        return Err(anyhow::anyhow!(
            "swarm still connecting: a {}-shard task leg exceeds the pre-mesh outbound buffer \
             ({} slots free); retry once a peer is connected",
            chunks.len(),
            state.pending_outbound.remaining()
        ));
    }
    let mut cache_frames = Vec::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        let shard = Shard {
            group: group.clone(),
            idx: u32::try_from(idx).expect("idx is bounded by the gate above"),
            total,
        };
        let chunk_body = MessageBody::new(*chunk).expect("a substring of a valid body is valid");
        let msg = Message::new_frame(ctx.swarm, ctx.author, kind.clone(), chunk_body)
            .with_shard(Some(shard))
            .signed(&signer);
        let bytes = Bytes::from(msg.serialize()?);
        if total > LOGGED_SHARD_GROUP_MAX_TOTAL {
            cache_frames.push(bytes.clone());
        }
        send_task_leg(state, ctx.sender, ctx.output, &msg, bytes, false, content).await?;
    }
    if !cache_frames.is_empty() {
        state.shard_cache.insert(group.clone(), cache_frames);
    }
    // Echo + ingest the logical leg once (one content leg toward the cap).
    let logical = Message::new_frame(ctx.swarm, ctx.author, kind, payload_body).with_id(logical_id);
    ctx.output.print_task(&logical, true);
    ingest_own_leg(state, &logical, ctx.output);
    Ok((logical.id.clone(), logical))
}

/// Worker-emit a `TaskStatusUpdate` for a task we're serving: resolve the
/// other party from the task record and push the status to it. A progress note
/// like `"35/100"` on a non-terminal `working` state rides as a beat fraction.
///
/// # Errors
/// `unknown task` if we hold no record for `task_id`; otherwise a
/// serialize/broadcast failure.
pub(crate) async fn emit_task_status(
    ctx: &HandlerCtx<'_>,
    task_id: &crate::a2a::TaskId,
    task_state: crate::a2a::TaskState,
    note: Option<&str>,
    state: &mut EventLoopState,
) -> anyhow::Result<Message> {
    let Some(peer) = state.tasks.get(task_id).map(|rec| rec.peer.clone()) else {
        return Err(anyhow::anyhow!("unknown task '{task_id}'"));
    };
    broadcast_task_status(ctx, &peer, task_id, task_state, note, state)
        .await
        .map(|(_id, msg)| msg)
}

/// Worker-emit a `TaskArtifactUpdate` (the result) for a task we're serving.
///
/// # Errors
/// `unknown task` if we hold no record for `task_id`; otherwise a
/// serialize/broadcast failure.
pub(crate) async fn emit_task_artifact(
    ctx: &HandlerCtx<'_>,
    task_id: &crate::a2a::TaskId,
    text: &str,
    file: Option<crate::blob::FileRef>,
    state: &mut EventLoopState,
) -> anyhow::Result<Message> {
    let Some(peer) = state.tasks.get(task_id).map(|rec| rec.peer.clone()) else {
        return Err(anyhow::anyhow!("unknown task '{task_id}'"));
    };
    broadcast_task_artifact(ctx, &peer, task_id, text, file, state)
        .await
        .map(|(_id, msg)| msg)
}

/// Seal a directed payload `body` to `to`'s published X25519 key. A directed
/// frame is **never** sent in plaintext — if the recipient's card / seal key has
/// not replicated yet, this errors rather than leaking the body. The Ed25519
/// frame signature (added later, over this ciphertext) authenticates the sender.
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
    crate::protocol::seal::seal_to_body(&key, body.as_str())
}

/// A worker-emitted `TaskStatusUpdate` (`a2a status`): compose the A2A status
/// payload and push it to `peer` fire-and-forget.
///
/// # Errors
/// Propagates a serialize/broadcast failure.
pub(crate) async fn broadcast_task_status(
    ctx: &HandlerCtx<'_>,
    peer: &Nickname,
    task_id: &crate::a2a::TaskId,
    task_state: crate::a2a::TaskState,
    note: Option<&str>,
    state: &mut EventLoopState,
) -> anyhow::Result<(MessageId, Message)> {
    let update = crate::a2a::gossip::status_update(ctx.swarm, task_id, task_state, note, None);
    let body = seal_directed(state, peer, &crate::a2a::gossip::payload_body(&update)?)?;
    let kind = MessageKind::A2aStatus {
        to: peer.clone(),
        task_id: task_id.clone(),
    };
    broadcast_task_frame(ctx, kind, body, true, state).await
}

/// A worker-emitted `TaskArtifactUpdate` (`a2a artifact`): compose the A2A
/// artifact payload and push it to `peer` fire-and-forget.
///
/// # Errors
/// Propagates a serialize/broadcast failure.
pub(crate) async fn broadcast_task_artifact(
    ctx: &HandlerCtx<'_>,
    peer: &Nickname,
    task_id: &crate::a2a::TaskId,
    text: &str,
    file: Option<crate::blob::FileRef>,
    state: &mut EventLoopState,
) -> anyhow::Result<(MessageId, Message)> {
    let parts = build_offload_parts(ctx.swarm, ctx.author, task_id, text, file, state).await?;
    let update = crate::a2a::gossip::artifact_update_parts(ctx.swarm, task_id, parts);
    let body = seal_directed(state, peer, &crate::a2a::gossip::payload_body(&update)?)?;
    let kind = MessageKind::A2aArtifact {
        to: peer.clone(),
        task_id: task_id.clone(),
    };
    broadcast_task_frame(ctx, kind, body, true, state).await
}

/// Resolve the parts of a `--file`-bearing leg: with no file, a single text
/// part (today's behavior); with a file, offload it over the blob channel into a
/// `Part.url` reference (+ the text part when non-empty). Lazily binds this
/// peer's blob server on the first offload. The heavy read+hash runs off the
/// event loop inside `blob::url_part`.
async fn build_offload_parts(
    swarm: &SwarmId,
    author: &Nickname,
    task_id: &crate::a2a::TaskId,
    text: &str,
    file: Option<crate::blob::FileRef>,
    state: &mut EventLoopState,
) -> anyhow::Result<Vec<crate::a2a::Part>> {
    let Some(file) = file else {
        return Ok(vec![crate::a2a::Part::text(text)]);
    };
    let lookups = swarm
        .as_str()
        .parse::<crate::protocol::swarm::Swarm>()
        .map_err(|error| anyhow::anyhow!("cannot resolve swarm lookups for blob offload: {error}"))?
        .lookups()
        .clone();
    // Route through the choke point so the base is validated (0700, ours) before
    // this attachment payload spool is created — bypassing it could birth the
    // shared base at a world-traversable 0755.
    let spool = crate::util::ensure_swarm_runtime_dir(swarm.as_str())
        .map_err(|error| anyhow::anyhow!("cannot prepare blob spool dir: {error}"))?
        .join(format!("{author}.blobs"));
    // Every offloaded blob inherits the swarm password (if any), so a scraped
    // ticket can't be redeemed without it.
    let password = state.swarm_password.clone();
    let part = crate::blob::url_part(
        file,
        &mut state.blob_server,
        &lookups,
        spool,
        task_id.clone(),
        password,
    )
    .await?;
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
    sender: &SwarmSender,
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
        crate::unicast::deliver(msg, bytes, state, sender).await?;
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
/// once if a content leg pushed the task past its whole-task cap.
fn ingest_own_leg(state: &mut EventLoopState, msg: &Message, out: &output::Output) {
    if crate::a2a::task::ingest(&mut state.tasks, msg, true, Instant::now()) {
        out.info(&format!(
            "task exceeded {} messages; wrap it up",
            crate::util::consts::TASK_CONTENT_CAP
        ));
        tracing::warn!("task content cap exceeded");
    }
}

/// Client side of the gossip A2A request/response: mint an `rpc_id`, park a
/// waiter (keyed by `rpc_id` + `peer`, with `timeout`), and broadcast an
/// `A2aReq` directed at `peer` carrying the JSON-RPC `{method, params}`. The
/// peer serves the safe method set and replies with an `A2aResp` that the
/// receive path routes into the waiter (`state.fulfill_a2a_waiter`); the
/// waiter times out via the loop's a2a-deadline arm. Fails fast (through the
/// responder) when `peer` isn't a current participant or the waiter registry
/// is full — no silent park.
pub(crate) async fn broadcast_a2a_call(
    ctx: &HandlerCtx<'_>,
    peer: Nickname,
    method: &str,
    params: serde_json::Value,
    timeout: std::time::Duration,
    responder: crate::daemon::state::A2aResponder,
    state: &mut EventLoopState,
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
    let party_to_a_task = state.tasks.values().any(|rec| rec.peer == peer);
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
    let rpc_id = crate::a2a::A2aRpcId::random();
    let kind = MessageKind::A2aReq {
        to: peer.clone(),
        rpc_id: rpc_id.clone(),
    };
    // Clamp before adding to `Instant`: an unclamped client `timeout_secs` would
    // overflow the platform `Instant` and panic the event loop.
    let max_timeout =
        std::time::Duration::from_secs(crate::util::consts::A2A_CALL_MAX_TIMEOUT_SECS);
    if timeout > max_timeout {
        tracing::warn!(
            requested_secs = timeout.as_secs(),
            capped_secs = crate::util::consts::A2A_CALL_MAX_TIMEOUT_SECS,
            "a2a call timeout clamped to the maximum"
        );
    }
    let deadline = tokio::time::Instant::now() + timeout.min(max_timeout);
    if let Some(unregistered) = state.register_a2a_waiter(rpc_id, peer, deadline, responder) {
        unregistered.send_response(&rpc_error(-32603, "too many in-flight a2a calls"));
        return;
    }
    // Directed request over the shared RPC sender: unicast → circuit → gossip
    // per frame, splitting a large sealed body transparently.
    if let Err(error) = send_directed_rpc(ctx.swarm, ctx.author, kind, body, state, ctx.sender).await
    {
        tracing::warn!(target: "agent_gossip::gossip", %error, "a2a request send failed");
    }
}

/// Ask the authors of our stalled big (unlogged) partial groups to re-send
/// the missing shards — the repair half of big-group reliability (the author
/// half is `state.shard_cache`). Runs on the prune tick; fire-and-forget:
/// no waiter is parked, the repair *is* the retry (the next tick asks again
/// while the group survives its TTL). A missing card (can't seal) or send
/// failure just waits for the next round.
pub(crate) async fn send_shard_repair_requests(
    swarm: &SwarmId,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &SwarmSender,
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
        let kind = MessageKind::A2aReq {
            to: ticket.author.clone(),
            rpc_id: crate::a2a::A2aRpcId::random(),
        };
        tracing::debug!(
            target: "agent_gossip::gossip",
            author = %ticket.author,
            group = %ticket.group,
            missing = ticket.missing.len(),
            "requesting shard repair"
        );
        if let Err(error) = send_directed_rpc(swarm, author, kind, sealed, state, sender).await {
            tracing::debug!(%error, "shard repair request send failed; next tick retries");
        }
    }
}

/// Send one directed RPC frame (`A2aReq`/`A2aResp`), transparently splitting a
/// sealed body too large for one frame into shard-tagged frames — each an
/// ordinary directed frame riding the unicast → circuit → gossip cascade, so
/// the RPC plane is size-transparent like chat and task legs. RPC frames are
/// plumbing (never logged), so the shards reassemble only in the receiver's
/// dedicated store; the group id is a fresh uuid, single-purpose (the `rpc_id`
/// stays the correlation key).
///
/// # Errors
/// Propagates a serialize/deliver failure and refuses a body past the input
/// ceiling or the receiver's per-group reassembly budget.
pub(crate) async fn send_directed_rpc(
    swarm: &SwarmId,
    author: &Nickname,
    kind: MessageKind,
    body: MessageBody,
    state: &mut EventLoopState,
    sender: &SwarmSender,
) -> anyhow::Result<()> {
    let signer = state.identity.clone();
    let single = Message::new_frame(swarm, author, kind.clone(), body.clone()).signed(&signer);
    if single.wire_len() <= MAX_MESSAGE_SIZE {
        crate::logging::messages::log_out(&single);
        let bytes = Bytes::from(single.serialize()?);
        return crate::unicast::deliver(&single, bytes, state, sender).await;
    }
    // The RPC body is already sealed (base58, ~1.37x the input) — gate on the
    // sealed ceiling so the caller-facing limit stays MAX_LOGICAL_BODY_BYTES.
    if body.as_str().len() > crate::util::consts::MAX_SEALED_BODY_BYTES {
        return Err(anyhow::anyhow!(
            "rpc body too large: {} sealed bytes exceeds the input ceiling \
             ({MAX_LOGICAL_BODY_BYTES} raw bytes); use the blob channel for files",
            body.as_str().len()
        ));
    }
    let group = ShardGroup::from_uuid_str(&uuid::Uuid::new_v4().to_string())
        .expect("a freshly minted uuid is a valid shard group");
    let probe = Message::new_frame(
        swarm,
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
    let chunks = split_body(body.as_str(), budget)
        .ok_or_else(|| anyhow::anyhow!("shard body budget is zero; cannot split"))?;
    let total = gate_multipart(&chunks, "rpc body")?;
    let mut cache_frames = Vec::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        let chunk_body = MessageBody::new(*chunk).expect("a substring of a valid body is valid");
        let msg = Message::new_frame(swarm, author, kind.clone(), chunk_body)
            .with_shard(Some(Shard {
                group: group.clone(),
                idx: u32::try_from(idx).expect("idx is bounded by the gate above"),
                total,
            }))
            .signed(&signer);
        crate::logging::messages::log_out(&msg);
        let bytes = Bytes::from(msg.serialize()?);
        if total > LOGGED_SHARD_GROUP_MAX_TOTAL {
            cache_frames.push(bytes.clone());
        }
        crate::unicast::deliver(&msg, bytes, state, sender).await?;
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
    ctx: &HandlerCtx<'_>,
    state: &mut EventLoopState,
    resp: tokio::sync::oneshot::Sender<Vec<output::PingPeer>>,
) -> bool {
    let now = tokio::time::Instant::now();
    state.ping_round = Some(Box::new(crate::daemon::state::PingRound {
        t1: now,
        deadline: now + std::time::Duration::from_secs(crate::util::tuning::ping_window_secs()),
        pongs: std::collections::HashMap::new(),
        resp: Some(resp),
    }));
    broadcast_msg(
        ctx.sender,
        &Message::new_ping(ctx.swarm, ctx.author).signed(&state.identity),
    )
    .await;
    true
}

/// Handle one typed in-process [`SessionRequest`] (embed / MCP). `Send`
/// broadcasts via the shared helper and echoes the canonical [`Message`]
/// back on the oneshot; `Poll` returns the join-horizon-filtered buffer.
/// Returns `true` if anything was broadcast so the caller can refresh
/// Deliver a task-leg outcome on its oneshot and report whether anything hit
/// the wire (the caller refreshes the heartbeat clock on `true`).
fn respond_with(
    resp: tokio::sync::oneshot::Sender<anyhow::Result<Message>>,
    outcome: anyhow::Result<Message>,
) -> bool {
    let sent_ok = outcome.is_ok();
    let _ = resp.send(outcome);
    sent_ok
}

/// `last_sent_at` (mirrors `handle_ipc_command`).
pub(crate) async fn handle_session_request(
    req: SessionRequest,
    ctx: &HandlerCtx<'_>,
    state: &mut EventLoopState,
) -> bool {
    match req {
        SessionRequest::Send { body, resp } => {
            let outcome =
                broadcast_message(ctx.swarm, ctx.author, body, state, ctx.sender, ctx.output)
                    .await
                    .map(|(_id, msg)| msg);
            let sent_ok = outcome.is_ok();
            let _ = resp.send(outcome);
            sent_ok
        }
        // Same policy as the CLI/IPC `Poll` arm: respond now if events are
        // buffered, else (with `long`) park a typed waiter the loop
        // fulfills/expires. A parked waiter broadcasts nothing → `false`.
        SessionRequest::Poll { after, long, resp } => {
            state.poll_or_register(
                after,
                long,
                tokio::time::Instant::now(),
                crate::daemon::state::PollResponder::Typed(resp),
            );
            false
        }
        SessionRequest::TaskStatus {
            task_id,
            state: task_state,
            note,
            resp,
        } => respond_with(
            resp,
            emit_task_status(ctx, &task_id, task_state, note.as_deref(), state).await,
        ),
        SessionRequest::TaskArtifact {
            task_id,
            text,
            file,
            resp,
        } => respond_with(resp, emit_task_artifact(ctx, &task_id, &text, file, state).await),
        SessionRequest::Peers { resp } => {
            let _ = resp.send(state.roster_snapshot());
            false
        }
        SessionRequest::StateMerge { merge, resp } => {
            let outcome = broadcast_state_merge(ctx, merge, state, Channel::State, true).await;
            let sent = outcome.is_ok();
            let _ = resp.send(outcome);
            sent
        }
        SessionRequest::StateGet { resp } => {
            let _ = resp.send(state.state_doc.to_json());
            false
        }
        SessionRequest::MetaMerge { merge, resp } => {
            let outcome = broadcast_state_merge(ctx, merge, state, Channel::Meta, true).await;
            let sent = outcome.is_ok();
            let _ = resp.send(outcome);
            sent
        }
        SessionRequest::MetaGet { resp } => {
            let _ = resp.send(state.meta_doc.to_json());
            false
        }
        SessionRequest::Ping { resp } => session_ping(ctx, state, resp).await,
        SessionRequest::A2aCall {
            peer,
            method,
            params,
            timeout,
            resp,
        } => {
            broadcast_a2a_call(
                ctx,
                peer,
                &method,
                params,
                timeout,
                crate::daemon::state::A2aResponder::Typed(resp),
                state,
            )
            .await;
            true
        }
        // Raw injection (adversarial only): broadcast the bytes verbatim, no
        // signing or chain stamping — a malicious/crafted message on the wire.
        #[cfg(feature = "adversarial")]
        SessionRequest::InjectRaw { bytes } => {
            let _ = ctx.sender.broadcast(bytes).await;
            true
        }
        #[cfg(feature = "adversarial")]
        SessionRequest::InjectLinkVector {
            origin,
            seq,
            seal_key,
            links,
        } => {
            state
                .link_state
                .ingest(crate::circuit::LinkVector::from_raw(
                    origin, seq, seal_key, links,
                ));
            false
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
        // Reassembly accounting snapshot (adversarial only) for the
        // shard-budget tripwires.
        #[cfg(feature = "adversarial")]
        SessionRequest::ReassemblyStats { resp } => {
            let _ = resp.send(state.reassembly.stats());
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

#[cfg(test)]
mod split_body_tests {
    use super::{escaped_char_len, gate_multipart, split_body};
    use crate::util::consts::{MAX_SHARD_TOTAL, REASSEMBLY_GROUP_MAX_BYTES};

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
        let chunks = split_body(&body, 16).expect("a positive budget always splits");
        assert!(chunks.len() > 1, "the body must actually split");
        assert_eq!(chunks.concat(), body, "chunks reassemble to the original");
        for chunk in &chunks {
            let escaped: usize = chunk.chars().map(escaped_char_len).sum();
            assert!(escaped <= 16, "each chunk's escaped length fits the budget");
        }
    }

    #[test]
    fn splits_multibyte_on_char_boundaries() {
        let body = "héllo🌍".repeat(8); // 2-byte é and 4-byte emoji
        let chunks = split_body(&body, 8).expect("a positive budget always splits");
        assert_eq!(chunks.concat(), body, "no char is split mid-codepoint");
        assert!(chunks.iter().all(|chunk| !chunk.is_empty()));
    }

    #[test]
    fn split_has_no_count_cap_but_the_gate_bounds_bytes() {
        // 1000 one-byte chunks was over the old 16-shard cap; the split is now
        // unbounded and the gate admits it (well under the byte budgets)…
        let body = "x".repeat(1000);
        let chunks = split_body(&body, 1).expect("no shard-count refusal");
        assert_eq!(chunks.len(), 1000);
        assert!(gate_multipart(&chunks, "message").is_ok());
        // …but the gate refuses what the receiver's budgets would refuse.
        let too_many: Vec<&str> = vec!["x"; MAX_SHARD_TOTAL as usize + 1];
        assert!(gate_multipart(&too_many, "message").is_err());
        let big = "x".repeat(REASSEMBLY_GROUP_MAX_BYTES / 2 + 1);
        let over_budget: Vec<&str> = vec![&big, &big];
        assert!(gate_multipart(&over_budget, "message").is_err());
    }
}
