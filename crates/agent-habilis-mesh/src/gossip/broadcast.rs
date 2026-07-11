//! Gossip **outbound/send plane** (engine primitives): fire-and-forget
//! broadcast, the shared-state merge write, and presence / `PeerInfo`
//! announcements. The application payload send path (broadcast chat, task
//! legs, directed RPC) lives app-side in the a2a send plane; this layer only
//! carries the generic transport primitives the app rides. Inbound dispatch
//! lives in [`super::recv`].

use crate::transport::MeshSender;
use bytes::Bytes;

use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::gossip::event::{NodeEvent, NodeSink};
use crate::protocol::message::AppFrameParams;
use crate::protocol::{Channel, MeshId, Message, MessageBody, Nickname};
use crate::util::consts::MAX_MESSAGE_SIZE;

/// Build → sign → deliver one outbound `App` frame — the payload-generic
/// counterpart to the a2a layer's own (chain-stamping, sharding, sealing) chat /
/// task builders. A non-a2a consumer emits its `App`-tagged payloads through
/// this one primitive: it stamps the frame with `tag`/`to`/`corr`, signs it with
/// the loop identity, and routes it with the same single-send decision the a2a
/// path uses ([`crate::unicast::deliver`]) — unicast to a sole addressee
/// when dialable, gossip otherwise. A broadcast (`to == None`) always gossips.
///
/// Reliability matches the engine's own plumbing: while unmeshed the frame is
/// buffered into `pending_outbound` (flushed on the first real-peer link), so a
/// send issued before the mesh forms is not lost.
///
/// This is a **single-frame** send: unlike the a2a chat path it does not split a
/// large body across shards (that logic is entangled with a2a's per-author hash
/// chain), so `body` must fit one wire frame. A body whose signed frame would
/// exceed [`MAX_MESSAGE_SIZE`] is refused rather than silently dropped by the
/// gossip actor; a bytes-carrying consumer chunks its stream below that bound.
/// It also does **not** seal a directed frame — the recipient's encryption key
/// is discovered through an app-level convention (a2a publishes cards to the
/// meta channel) the engine does not own; a directed `App` frame sent here rides
/// in plaintext (authenticated by the Ed25519 frame signature).
///
/// Nor does it seal a **broadcast** body: on a passworded mesh (a
/// `broadcast_key` is set), receivers require every broadcast body to be
/// mesh-key-sealed and drop a plaintext one on decrypt (see
/// `gossip::recv::decrypt_broadcast`). A generic consumer that needs passworded
/// broadcast must therefore seal its body under the mesh key before calling
/// this; a2a already does so on its broadcast chat path.
///
/// # Errors
/// Refuses a body too large for one frame, propagates a serialize/deliver
/// failure, and errors if the unmeshed buffer is full.
pub async fn send_app(
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
    params: AppFrameParams,
) -> anyhow::Result<()> {
    let frame = Message::new_app(ctx.mesh, ctx.author, params).signed(ctx.identity);
    if frame.wire_len() > MAX_MESSAGE_SIZE {
        anyhow::bail!(
            "app frame too large: {} bytes exceeds the {MAX_MESSAGE_SIZE}-byte single-frame limit",
            frame.wire_len()
        );
    }
    let bytes = Bytes::from(frame.serialize()?);
    crate::logging::messages::log_out(&frame);
    if state.meshed {
        // Directed vs. broadcast is decided inside `deliver` from the frame's
        // `to` — the same routing the a2a send path rides.
        crate::unicast::deliver(&frame, bytes, state, ctx.sender).await
    } else if state.pending_outbound.push(bytes.clone()) {
        // Buffered until the first real-peer link flushes it (see
        // `gossip::recv::flush_pending`).
        Ok(())
    } else {
        anyhow::bail!("pending outbound buffer full; app frame dropped")
    }
}

/// Fire-and-forget gossip broadcast. Serialize errors are swallowed:
/// this helper is for presence / `PeerInfo` announcements where a
/// failed serialize must not block the daemon. A failed *broadcast* is
/// logged — it means the gossip actor refused the send (the wedge the
/// roster-collapse soak hit silently), not a routine empty room.
pub async fn broadcast_msg(sender: &MeshSender, msg: &Message) {
    crate::logging::messages::log_out(msg);
    if let Ok(bytes) = msg.serialize()
        && let Err(error) = sender.broadcast(Bytes::from(bytes)).await
    {
        tracing::warn!(
            target: "agent_square::gossip",
            %error,
            "presence/plumbing broadcast failed"
        );
    }
}

/// Parameters for [`broadcast_state_merge`]. A dedicated struct rather than
/// reusing `HandlerCtx`: several root-crate callers only hold loose
/// `mesh`/`author`/`sender`/`sink` locals, not a full `HandlerCtx`.
#[expect(
    missing_debug_implementations,
    reason = "sink is a &'a dyn NodeSink trait object with no Debug impl"
)]
pub struct StateMergeParams<'a> {
    pub mesh: &'a MeshId,
    pub author: &'a Nickname,
    pub merge: serde_json::Value,
    pub sender: &'a MeshSender,
    pub sink: &'a dyn NodeSink,
    pub channel: Channel,
    pub surface: bool,
}

/// The single shared-state write helper, shared by the IPC `state_merge` command
/// and the in-process `StateMerge` request. Translates the RFC 7386 merge into one
/// automerge change, gossips it inside a signed frame, and retains it locally so
/// anti-entropy can serve it (gossip never echoes to self).
///
/// The change is built on a fork and the frame size-gated **before** the change
/// touches the live doc, so an oversize merge never lands in a doc it could not
/// be gossiped for. Applying then runs through [`MeshDoc::ingest`]'s gate, so a
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
pub async fn broadcast_state_merge(
    state: &mut EventLoopState,
    params: StateMergeParams<'_>,
) -> anyhow::Result<()> {
    use crate::daemon::doc::Ingested;

    let StateMergeParams {
        mesh,
        author,
        merge,
        sender,
        sink,
        channel,
        surface,
    } = params;

    // 1. Build the change on a fork (no live mutation yet); a no-op merge is a
    //    silent success.
    let built = match channel {
        Channel::State => state.state_doc.build_change(&merge, author)?,
        Channel::Meta => state.meta_doc.build_change(&merge, author)?,
    };
    let Some(change_bytes) = built else {
        return Ok(());
    };

    // 2. Compose + sign + size-gate before the change touches the live doc.
    //    Carry the input merge as the surfaced delta only for agent-visible
    //    writes; the internal card publish stays lean (no delta on the wire).
    //    On a passworded mesh the wire body is sealed under the channel key;
    //    `plain_body` keeps the plaintext for our own surfacing below.
    let (wire_body, plain_body) = match channel {
        Channel::State => state
            .state_doc
            .compose_wire_body(&change_bytes, surface.then_some(&merge))?,
        Channel::Meta => state
            .meta_doc
            .compose_wire_body(&change_bytes, surface.then_some(&merge))?,
    };
    let signed =
        Message::new_channel_event(mesh, author, wire_body, channel).signed(&state.identity);
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
        sink.emit(NodeEvent::StateChanged {
            channel,
            event: Box::new(surfaced),
            document: after,
            is_self: true,
        });
    }
    if state.meshed {
        sender
            .broadcast(Bytes::from(bytes))
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
    } else {
        // Unmeshed: buffer for gossip until we mesh. (The change is already in
        // the local doc, so heads anti-entropy still reconciles it if the buffer
        // is full.)
        state.pending_outbound.push(Bytes::from(bytes));
    }
    Ok(())
}

/// Broadcast a `PeerInfo` carrying our endpoint address so peers can
/// dial us directly. Unlike `joined`, `PeerInfo` never enters the
/// message log (`handle_peer_info` returns before the log push), so
/// re-sending it is invisible to `poll`/`fetch_messages` consumers —
/// safe to repeat on every new neighbor.
pub(super) async fn broadcast_peer_info(ctx: &HandlerCtx<'_>) {
    let our_addr = ctx.endpoint.addr();
    let addr_data = serde_json::to_string(&crate::protocol::peer_addr::endpoint_addr_to_json(
        &our_addr,
    ))
    .expect("endpoint_addr_to_json produces a Value that always serializes");
    let addr_body =
        MessageBody::new(addr_data).expect("endpoint address JSON has no control characters");
    broadcast_msg(
        ctx.sender,
        &Message::new_peer_info(ctx.mesh, ctx.author, addr_body).signed(ctx.identity),
    )
    .await;
}

/// Announce our arrival: `joined` presence followed by `PeerInfo`.
/// Called once at the top of the event loop; the caller bumps
/// `last_sent_at` afterwards.
pub(super) async fn announce_arrival(ctx: &HandlerCtx<'_>) {
    broadcast_msg(
        ctx.sender,
        &Message::new_joined(ctx.mesh, ctx.author).signed(ctx.identity),
    )
    .await;
    broadcast_peer_info(ctx).await;
}
