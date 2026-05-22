//! The **gossip subsystem**: the message transport plane. Outbound
//! broadcast (and the unmeshed-join buffer), the inbound gossip-event
//! pump, neighbor up/down bookkeeping, and the per-message router.
//! Membership/presentation lives in `lifecycle`; anti-entropy and the
//! healer are the `antientropy` / `heal` submodules. This layer never
//! touches the participant roster directly — it calls into
//! `lifecycle::observe` and dispatches by kind.

pub(crate) mod antientropy;
pub(crate) mod heal;

use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh::endpoint::TransportAddrUsage;
use iroh::{Endpoint, EndpointId, RelayUrl, TransportAddr};
use iroh_gossip::api::{ApiError, Event, GossipSender};

use crate::daemon::SendRequest;
use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::discovery::add_peer_addr;
use crate::lifecycle;
use crate::output;
use crate::protocol::{Message, MessageBody, MessageId, MessageKind, Nickname, SwarmId};
use crate::util::tuning::RECLAIM_WINDOW_SECS;

/// Snapshot the active transport path to `node_id`: a short label
/// (`direct` / `relay` / `mixed` / `unknown`) plus the relay URL when
/// one is in use. Point-in-time, not a watcher — iroh starts a fresh
/// link relayed and upgrades to direct after hole-punching, so a label
/// taken right at `NeighborUp` skews toward `relay`; the periodic
/// census reading is the representative one. Diagnostics only — the
/// most-requested observability gap across iroh apps (sendme #67/#112,
/// psyche #586). See docs/iroh-ecosystem-research.md.
pub(crate) async fn conn_path(
    endpoint: &Endpoint,
    node_id: EndpointId,
) -> (&'static str, Option<RelayUrl>) {
    let Some(info) = endpoint.remote_info(node_id).await else {
        return ("unknown", None);
    };
    let mut has_direct = false;
    let mut has_relay = false;
    let mut relay_url = None;
    for addr in info.addrs() {
        if !matches!(addr.usage(), TransportAddrUsage::Active) {
            continue;
        }
        match addr.addr() {
            TransportAddr::Relay(url) => {
                has_relay = true;
                relay_url = Some(url.clone());
            }
            TransportAddr::Ip(_) => has_direct = true,
            _ => {}
        }
    }
    let label = match (has_direct, has_relay) {
        (true, true) => "mixed",
        (true, false) => "direct",
        (false, true) => "relay",
        (false, false) => "unknown",
    };
    (label, relay_url)
}

/// Fire-and-forget gossip broadcast. Serialize errors are swallowed:
/// this helper is for presence / `PeerInfo` announcements where a
/// failed serialize must not block the daemon.
pub(crate) async fn broadcast_msg(sender: &GossipSender, msg: &Message) {
    crate::messages::log_out(msg);
    if let Ok(bytes) = msg.serialize() {
        let _ = sender.broadcast(Bytes::from(bytes)).await;
    }
}

/// Broadcast a `PeerInfo` carrying our endpoint address so peers can
/// dial us directly. Unlike `joined`, `PeerInfo` never enters the
/// message log (`handle_peer_info` returns before the log push), so
/// re-sending it is invisible to `poll`/`fetch_messages` consumers —
/// safe to repeat on every new neighbor.
async fn broadcast_peer_info(
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    endpoint: &Endpoint,
) {
    let our_addr = endpoint.addr();
    let addr_data = serde_json::to_string(&crate::protocol::peer_addr::endpoint_addr_to_json(
        &our_addr,
    ))
    .expect("endpoint_addr_to_json produces a Value that always serializes");
    let addr_body =
        MessageBody::new(addr_data).expect("endpoint address JSON has no control characters");
    broadcast_msg(sender, &Message::new_peer_info(swarm, author, addr_body)).await;
}

/// Announce our arrival: `joined` presence followed by `PeerInfo`.
/// Called once at the top of the event loop; the caller bumps
/// `last_sent_at` afterwards.
async fn announce_arrival(
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    endpoint: &Endpoint,
) {
    broadcast_msg(sender, &Message::new_joined(swarm, author)).await;
    broadcast_peer_info(sender, swarm, author, endpoint).await;
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

/// Send `bytes` to the swarm, or buffer it if we have no gossip link
/// yet. Before the first `NeighborUp` a bare `broadcast` goes into the
/// void (no eager-push peers) and is a lost one-shot; queueing it and
/// flushing on connect makes the first message after a join reliable.
/// Returns `Err` only on a genuine broadcast failure (queued = `Ok`).
async fn emit_or_queue(
    state: &mut EventLoopState,
    sender: &GossipSender,
    bytes: Bytes,
    out: &output::Output,
) -> anyhow::Result<()> {
    if state.meshed {
        let result = sender
            .broadcast(bytes)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"));
        tracing::trace!(ok = result.is_ok(), "broadcast to mesh");
        return result;
    }
    if state.queue_outbound(bytes).is_some() {
        out.info("pending outbound buffer full; dropped oldest undelivered message");
        tracing::warn!("pending outbound buffer full; dropped oldest undelivered message");
    } else {
        tracing::trace!("queued outbound message (unmeshed)");
    }
    Ok(())
}

/// Build, sign, log and gossip-broadcast one outbound message. The
/// single source of truth for the send path: the IPC `Msg` command
/// and the embed facade's `external_send_rx` arm both funnel through
/// here so they cannot drift. Returns the new id and the canonical
/// `Message` so callers can echo it without re-parsing.
///
/// The caller refreshes `state.last_sent_at` on success.
pub(crate) async fn broadcast_message(
    swarm: &SwarmId,
    author: &Nickname,
    body: MessageBody,
    reply: Option<Nickname>,
    state: &mut EventLoopState,
    sender: &GossipSender,
    out: &output::Output,
) -> anyhow::Result<(MessageId, Message)> {
    let (bytes, msg) = crate::protocol::message::build_msg_bytes(swarm, body, reply, author)?;
    let id = msg.id.clone();
    out.print_message_ex(&msg, true);
    state.message_log.push(msg.clone());
    crate::messages::log_out(&msg);
    emit_or_queue(state, sender, bytes, out).await?;
    Ok((id, msg))
}

/// Dispatch a single item from the gossip receiver stream:
/// `Received` → `handle_gossip_received`; `NeighborUp` →
/// announce / `PeerInfo` re-send; `NeighborDown` → prune the link and
/// arm reclaim; `Lagged` / errors logged; terminal `None` flips
/// `state.gossip_open` to `false`.
pub(crate) async fn handle_gossip_event(
    event: Option<Result<Event, ApiError>>,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    match event {
        Some(Ok(Event::Received(received))) => {
            handle_gossip_received(received.content, state, ctx).await;
        }
        Some(Ok(Event::NeighborUp(node_id))) => {
            let (conn, relay) = conn_path(ctx.endpoint, node_id).await;
            tracing::info!(
                endpoint_id = %node_id,
                is_rendezvous = node_id == ctx.rendezvous_id,
                conn,
                relay = relay.as_ref().map_or("-", |url| url.as_str()),
                "gossip neighbor up"
            );
            // Announce on the first link, not at startup: a joiner's
            // first link is the rendezvous relay, and `joined` is a
            // one-shot — sending it before any link exists loses it.
            // Later neighbors only get a `PeerInfo` re-send.
            if state.announced {
                broadcast_peer_info(ctx.sender, ctx.swarm, ctx.author, ctx.endpoint).await;
            } else {
                announce_arrival(ctx.sender, ctx.swarm, ctx.author, ctx.endpoint).await;
                state.announced = true;
                tracing::info!("announced arrival on first gossip link");
            }
            state.last_sent_at = Instant::now();
            // The co-hosted rendezvous is overlay plumbing, not a
            // participant — never cache its pseudo-node in the
            // bootstrap set. Transport links are not surfaced at all:
            // arrival is surfaced once, via membership presence
            // (`joined`), keyed by nickname and join-horizon gated.
            if node_id != ctx.rendezvous_id {
                // First link to a *real* peer: now (and only now) can
                // user content actually be delivered. Flush anything
                // buffered while we were unmeshed, in order.
                if !state.meshed {
                    state.meshed = true;
                    let mut flushed = 0usize;
                    for bytes in state.take_pending_outbound() {
                        let _ = ctx.sender.broadcast(bytes).await;
                        flushed += 1;
                    }
                    tracing::info!(
                        flushed,
                        "meshed: first real-peer link up, flushed buffered messages"
                    );
                }
            }
        }
        Some(Ok(Event::NeighborDown(node_id))) => {
            let is_rendezvous = node_id == ctx.rendezvous_id;
            tracing::info!(endpoint_id = %node_id, is_rendezvous, "gossip neighbor down");
            if !is_rendezvous {
                state.linked_endpoints.remove(&node_id);
            }
            // Arm the fast reclaim burst only on a real beacon-loss /
            // isolation signal: the rendezvous link dropped, or that
            // was our last tracked peer. A plain HyParView shuffle (a
            // participant flapping while others remain) must NOT arm
            // it, or initial multi-node convergence pays a needless
            // ~6s bind storm on every non-beacon node.
            if is_rendezvous || state.linked_endpoints.is_empty() {
                state.reclaim_until =
                    Some(Instant::now() + Duration::from_secs(RECLAIM_WINDOW_SECS));
                tracing::info!(
                    reason = if is_rendezvous {
                        "rendezvous-loss"
                    } else {
                        "last-peer"
                    },
                    "armed fast reclaim window"
                );
            }
        }
        Some(Ok(Event::Lagged)) => {
            ctx.output
                .info("Event stream lagged, some messages may have been missed");
            tracing::warn!("gossip event stream lagged; some messages missed");
        }
        Some(Err(error)) => {
            ctx.output.error(&format!("Gossip error: {error}"));
            tracing::warn!(%error, "gossip error");
        }
        None => {
            // Stream ended. IPC keeps working for msg/poll.
            state.gossip_open = false;
            tracing::warn!("gossip stream ended; IPC msg/poll still works");
        }
    }
}

/// Handle a single received gossip payload: parse, drop self-echo,
/// rate-check, run the lifecycle observer (heartbeat / membership /
/// surfacing / horizon), dispatch by kind, and finally push to the
/// message log if loggable.
async fn handle_gossip_received(content: Bytes, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
    let Ok(message) = Message::parse(&content) else {
        ctx.output.error("Failed to parse message");
        tracing::warn!("failed to parse inbound gossip message");
        return;
    };
    if message.author == *ctx.author {
        return;
    }
    tracing::trace!(author = %message.author, "gossip message received");
    // Duplicate suppression: a true repeat delivery must not
    // re-rate-count, re-heartbeat, re-run membership, re-embed-forward,
    // re-log, or re-print. Re-broadcasts of `joined`/`Alive` mint fresh
    // ids so they are never falsely suppressed here.
    if state.mark_seen(&message.id) {
        return;
    }
    let rate_ok = match &message.kind {
        MessageKind::Msg { reply: None } => state.rate_limiter.check_message(&message.author),
        MessageKind::Msg { reply: Some(_) } => state.rate_limiter.check_reply(&message.author),
        _ => true,
    };
    if !rate_ok {
        let notice = format!("rate limit exceeded for [{}], dropping", message.author);
        ctx.output.info(&notice);
        tracing::debug!(author = %message.author, "rate limit exceeded; dropping");
        return;
    }

    // Heartbeat + membership + surfacing + join horizon. The lifecycle
    // layer owns every roster/presentation side effect; the gossip
    // layer only routes by kind below.
    crate::messages::log_in(&message);
    let observed = lifecycle::observe(&message, state, ctx);
    let surfaceable = observed.surfaceable;

    // Embed push: hand every surviving inbound message to the facade
    // before kind routing, so the consumer sees msg / presence /
    // peer_info alike. Non-blocking by construction (bounded
    // broadcast); a send error or full ring is intentionally dropped
    // so a slow embedder never stalls the gossip loop. The
    // receiver_count gate skips the per-message clone while no
    // consumer is subscribed (always, until the embedder calls
    // messages(); forever for CLI/MCP where the field is None).
    // `surfaceable` keeps pre-join backlog off the embed channel too.
    if let Some(tx) = ctx.external_msg_tx
        && tx.receiver_count() > 0
        && surfaceable
        && !matches!(message.kind, MessageKind::Digest)
    {
        let _ = tx.send(message.clone());
    }

    match &message.kind {
        MessageKind::PeerInfo => {
            handle_peer_info(&message, content, state, ctx).await;
            return;
        }
        MessageKind::Digest => {
            antientropy::handle_digest(&message, state, ctx).await;
            return;
        }
        MessageKind::Presence { subtype } => {
            lifecycle::handle_presence(
                &message,
                *subtype,
                &observed.update,
                surfaceable,
                state,
                ctx,
            )
            .await;
        }
        MessageKind::Msg { .. } => {
            if !lifecycle::handle_msg(ctx.output, &message, surfaceable, ctx.author) {
                return;
            }
        }
    }

    if is_loggable(&message.kind) {
        state.message_log.push(message);
    }
}

async fn handle_peer_info(
    message: &Message,
    content: Bytes,
    state: &mut EventLoopState,
    ctx: &HandlerCtx<'_>,
) {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(message.body.as_str()) else {
        return;
    };
    let Ok((peer_id, peer_addr)) = crate::protocol::peer_addr::endpoint_addr_from_json(&parsed)
    else {
        return;
    };
    if peer_id != ctx.endpoint.id()
        && peer_id != ctx.rendezvous_id
        && state.linked_endpoints.len() < ctx.max_peers
        && state.linked_endpoints.insert(peer_id)
    {
        let _ = add_peer_addr(ctx.endpoint, peer_addr);
        let _ = ctx.sender.join_peers(vec![peer_id]).await;
        let _ = ctx.sender.broadcast(content).await;
        state.last_sent_at = Instant::now();
        tracing::debug!(
            endpoint_id = %peer_id,
            linked = state.linked_endpoints.len(),
            "linked new peer from PeerInfo"
        );
    }
}

/// Handle one embed `SendRequest`: broadcast via the shared helper and
/// reply on the oneshot. Returns `true` if anything was broadcast so
/// the caller can refresh `last_sent_at` (mirrors `handle_ipc_command`).
pub(crate) async fn handle_send_request(
    req: SendRequest,
    swarm: &SwarmId,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
) -> bool {
    let SendRequest { body, reply, resp } = req;
    let result = broadcast_message(swarm, author, body, reply, state, sender, output).await;
    let sent_ok = result.is_ok();
    let _ = resp.send(result.map(|(id, _msg)| id));
    sent_ok
}

/// `Alive` keepalives and anti-entropy `Digest`s are plumbing;
/// everything else goes in the log (and so to `poll`/`fetch`).
fn is_loggable(kind: &MessageKind) -> bool {
    !matches!(
        kind,
        MessageKind::Presence {
            subtype: crate::protocol::PresenceSubtype::Alive
        } | MessageKind::Digest
    )
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
mod is_loggable_tests {
    use super::is_loggable;
    use crate::protocol::{MessageKind, PresenceSubtype};

    #[test]
    fn alive_presence_is_not_loggable() {
        assert!(!is_loggable(&MessageKind::Presence {
            subtype: PresenceSubtype::Alive
        }));
    }

    #[test]
    fn joined_and_left_presence_are_loggable() {
        assert!(is_loggable(&MessageKind::Presence {
            subtype: PresenceSubtype::Joined
        }));
        assert!(is_loggable(&MessageKind::Presence {
            subtype: PresenceSubtype::Left
        }));
    }

    #[test]
    fn msg_peerinfo_are_loggable() {
        assert!(is_loggable(&MessageKind::Msg { reply: None }));
        assert!(is_loggable(&MessageKind::PeerInfo));
    }
}
