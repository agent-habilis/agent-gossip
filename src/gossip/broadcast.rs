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
use crate::protocol::identity::{self, Identity};
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
        Ok(SendOutcome::Sent(..)) => state.last_sent_at = Instant::now(),
        Ok(SendOutcome::RateLimited) => {}
        Err(error) => out.report_error(&error),
    }
}

/// Commit a just-built outbound `Msg` into local state: advance the per-author
/// hash chain (`seq`/`prev`) and the DAG tips, echo it to the operator, retain
/// it in the message log (pruning the fork/DAG indexes on eviction), and write
/// the dev log. Shared by the meshed and queued send paths so the chain
/// bookkeeping can't drift between them.
fn commit_outbound(state: &mut EventLoopState, msg: &Message, out: &output::Output) {
    let hash = msg.content_hash_hex();
    state.self_seq += 1;
    state.self_prev = Some(hash.clone());
    state.note_dag(hash, &msg.parents, msg.timestamp);
    out.print_message_ex(msg, true);
    if let Some(evicted) = state.message_log.push(msg.clone()) {
        let evicted_hash = evicted.content_hash_hex();
        state.forget_hash(&evicted_hash);
        if let Some(seq) = evicted.seq {
            state.forget_msg_seq(&evicted.pubkey, seq, &evicted_hash);
        }
    }
    crate::logging::messages::log_out(msg);
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
/// single source of truth for the send path: the CLI socket's IPC `Msg`
/// command and the typed in-process `SessionRequest::Send` both funnel
/// through here so they cannot drift. Rate-limited sends return
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
    // Our own signing identity + its pubkey: the rate limiter keys on the
    // verified pubkey (symmetric with the receiver, which keys on the
    // sender's signed pubkey), and `build_msg_bytes` signs with it. Clone
    // the Arc first so the immutable read is done before the `&mut state`
    // rate-limiter / log mutations below.
    let signer = state.identity.clone();
    let our_pubkey = identity::encode_pubkey(&signer.public());
    if !state.rate_limiter.check(&our_pubkey) {
        out.info(&format!("rate limit exceeded for [{author}], dropping"));
        tracing::debug!(%author, "send rate limit exceeded; not broadcasting");
        return Ok(SendOutcome::RateLimited);
    }
    // Stamp this Msg into our hash chain (Phase 2: seq + prev) and the
    // cross-author DAG (Phase 3: parents = the tips we've seen). After
    // building, advance the chain cursor and fold our own message into the
    // DAG so the next Msg back-links/parents here.
    let chain = crate::protocol::message::ChainCtx {
        seq: state.self_seq,
        prev: state.self_prev.clone(),
        parents: state.dag_parents(),
    };
    let (bytes, msg) =
        crate::protocol::message::build_msg_bytes(swarm, body, reply, author, &signer, chain)?;
    let id = msg.id.clone();
    if state.meshed {
        // Meshed: commit the chain + retain locally, then hit the wire. A
        // transient broadcast error still leaves the message in our log, so
        // anti-entropy can resend it.
        commit_outbound(state, &msg, out);
        sender
            .broadcast(bytes)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
    } else if state.pending_outbound.push(bytes) {
        // Unmeshed: buffered for flush-on-connect. Commit the chain now — the
        // message *will* be sent (in order) once we mesh.
        commit_outbound(state, &msg, out);
    } else {
        // Unmeshed AND the buffer is full: drop this send WITHOUT consuming a
        // seq, so the per-author chain stays contiguous. Dropping a queued
        // middle message would orphan its seq and leave peers a dangling
        // prev/parent that anti-entropy could never fill.
        out.info("pending outbound buffer full; outbound message dropped");
        tracing::warn!("pending outbound buffer full; outbound message dropped");
        return Ok(SendOutcome::RateLimited);
    }
    Ok(SendOutcome::Sent(id, msg))
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
            let outcome =
                broadcast_message(swarm, author, body, reply, state, sender, output).await;
            let sent_ok = matches!(outcome, Ok(SendOutcome::Sent(..)));
            let _ = resp.send(outcome.map(|sent| match sent {
                SendOutcome::Sent(_id, msg) => Some(msg),
                SendOutcome::RateLimited => None,
            }));
            sent_ok
        }
        SessionRequest::Poll { after, resp } => {
            let _ = resp.send(state.poll_after(after.as_ref(), output));
            false
        }
        // Raw injection (testkit only): broadcast the bytes verbatim, no
        // signing or chain stamping — a malicious/crafted message on the wire.
        #[cfg(feature = "testkit")]
        SessionRequest::InjectRaw { bytes } => {
            let _ = sender.broadcast(bytes).await;
            true
        }
        // Index-size snapshot (testkit only) for the leak regression test.
        #[cfg(feature = "testkit")]
        SessionRequest::IndexStats { resp } => {
            let _ = resp.send((
                state.by_hash.len(),
                state.dag_heads.len(),
                state.author_seqs.len(),
            ));
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
