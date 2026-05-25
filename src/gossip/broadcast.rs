//! Gossip **outbound/send plane**: building, signing, logging and
//! broadcasting messages; the unmeshed-join outbound buffer; presence /
//! `PeerInfo` announcements; and the interactive `/reply` stdin path.
//! [`broadcast_message`] is the single source of truth for the send
//! path — the IPC `Msg` command, the embed `SendRequest`, and stdin all
//! funnel through it so they cannot drift. Inbound dispatch lives in
//! [`super::recv`].

use std::time::Instant;

use bytes::Bytes;
use iroh::Endpoint;
use iroh_gossip::api::GossipSender;

use crate::daemon::SendRequest;
use crate::daemon::state::EventLoopState;
use crate::output;
use crate::protocol::{Message, MessageBody, MessageId, Nickname, SwarmId};

/// Fire-and-forget gossip broadcast. Serialize errors are swallowed:
/// this helper is for presence / `PeerInfo` announcements where a
/// failed serialize must not block the daemon.
pub(crate) async fn broadcast_msg(sender: &GossipSender, msg: &Message) {
    crate::logging::messages::log_out(msg);
    if let Ok(bytes) = msg.serialize() {
        let _ = sender.broadcast(Bytes::from(bytes)).await;
    }
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
pub(super) async fn announce_arrival(
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
        Ok(SendOutcome::Sent(..)) => state.last_sent_at = Instant::now(),
        Ok(SendOutcome::RateLimited) => {}
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

/// Outcome of a send through [`broadcast_message`]. `RateLimited` is a
/// *drop*, not an error (mirrors the receiver-side drop) — returned to
/// the caller so a programmatic sender (`ahs msg`, MCP `send_message`)
/// can tell the message was not emitted, distinct from a real failure.
#[expect(
    clippy::large_enum_variant,
    reason = "transient return value, never stored in bulk; boxing the common Sent payload to shrink the rare RateLimited unit variant would just add an allocation to every send"
)]
pub(crate) enum SendOutcome {
    Sent(MessageId, Message),
    RateLimited,
}

/// Build, sign, log and gossip-broadcast one outbound message. The
/// single source of truth for the send path: the IPC `Msg` command
/// and the embed facade's `external_send_rx` arm both funnel through
/// here so they cannot drift. Rate-limited sends return
/// [`SendOutcome::RateLimited`]; otherwise the new id and the canonical
/// `Message` so callers can echo it without re-parsing.
///
/// The caller refreshes `state.last_sent_at` only on `Sent`.
pub(crate) async fn broadcast_message(
    swarm: &SwarmId,
    author: &Nickname,
    body: MessageBody,
    reply: Option<Nickname>,
    state: &mut EventLoopState,
    sender: &GossipSender,
    out: &output::Output,
) -> anyhow::Result<SendOutcome> {
    // Sender-side rate limit, symmetric with the receiver check in
    // `recv::handle_gossip_received`: same limiter, same per-author
    // quota — applied to our own author so we never broadcast traffic
    // peers would only drop. Checked before build/print/log so a dropped
    // send leaves no trace, just as the receiver drops before
    // processing. The self bucket is never double-counted: self-authored
    // inbound is filtered out earlier.
    if !state.rate_limiter.check(author) {
        out.info(&format!("rate limit exceeded for [{author}], dropping"));
        tracing::debug!(%author, "send rate limit exceeded; not broadcasting");
        return Ok(SendOutcome::RateLimited);
    }
    let (bytes, msg) = crate::protocol::message::build_msg_bytes(swarm, body, reply, author)?;
    let id = msg.id.clone();
    out.print_message_ex(&msg, true);
    state.message_log.push(msg.clone());
    crate::logging::messages::log_out(&msg);
    emit_or_queue(state, sender, bytes, out).await?;
    Ok(SendOutcome::Sent(id, msg))
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
    let outcome = broadcast_message(swarm, author, body, reply, state, sender, output).await;
    let sent_ok = matches!(outcome, Ok(SendOutcome::Sent(..)));
    let _ = resp.send(outcome.map(|sent| match sent {
        SendOutcome::Sent(id, _msg) => Some(id),
        SendOutcome::RateLimited => None,
    }));
    sent_ok
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
