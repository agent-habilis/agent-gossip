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
use crate::protocol::{
    ExchangeId, ExchangeKind, ExchangePhase, Message, MessageBody, MessageId, MessageKind,
    Nickname, SwarmId,
};

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
/// state anti-entropy path could never serve it. The low-level primitive: it
/// does not itself rate-limit (the patch path [`broadcast_state_patch`] charges
/// the per-identity quota before calling in; the substrate membership path is
/// exempt) and skips the per-author `Msg` hash chain — state lives in its own
/// un-pruned log. When unmeshed the bytes are buffered for flush-on-connect;
/// even if that buffer is full the event is safe in the local state log, so
/// anti-entropy backfills peers once meshed.
///
/// # Errors
/// Serialization or broadcast failure.
pub(crate) async fn broadcast_state(
    swarm: &SwarmId,
    author: &Nickname,
    body: MessageBody,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
) -> anyhow::Result<()> {
    let signed = Message::new_state(swarm, author, body).signed(&state.identity);
    // Serialize **before** the local insert: an oversize body fails here and the
    // event never enters the log, so the author can't hold a patch it can never
    // gossip (anti-entropy can't resend an un-serializable event either) — that
    // would diverge permanently. A failed *broadcast* below still inserts and is
    // recoverable via anti-entropy; only a failed *serialize* is blocked.
    let bytes = signed.serialize()?;
    crate::logging::messages::log_out(&signed);
    let before = crate::daemon::state_doc::derive_document(&state.state_log);
    state.state_log.insert(signed.clone());
    let after = crate::daemon::state_doc::derive_document(&state.state_log);
    // Surface our own change (is_self) when the document actually changed — a
    // no-op patch (and non-patch substrate state) surfaces nothing.
    if after != before {
        output.state_changed(&signed, &after, true);
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

/// The outcome of an attempted shared-state write.
pub(crate) enum StatePatchOutcome {
    Applied,
    RateLimited,
    /// The patch is structurally bad (malformed / out of subset / doesn't apply)
    /// — a permanent failure; retrying the same patch won't help.
    Invalid(String),
    /// `--if-doc-hash` didn't match the current document — a **retryable**
    /// compare-and-set conflict (re-read and retry), distinct from `Invalid`.
    Stale(String),
}

/// Collapse a [`broadcast_state_patch`] outcome into the `Result<()>` the embed
/// `StatePatch` request returns: applied is `Ok`, every other case an error
/// (the invalid/stale reason verbatim, or `rate limited`).
fn state_patch_reply(outcome: anyhow::Result<StatePatchOutcome>) -> anyhow::Result<()> {
    match outcome {
        Ok(StatePatchOutcome::Applied) => Ok(()),
        Ok(StatePatchOutcome::Invalid(why) | StatePatchOutcome::Stale(why)) => {
            Err(anyhow::anyhow!(why))
        }
        Ok(StatePatchOutcome::RateLimited) => Err(anyhow::anyhow!("rate limited")),
        Err(error) => Err(error),
    }
}

/// The single shared-state write helper, shared by the IPC `state_patch`
/// command and the embed `StatePatch` request. Validates the patch against the
/// current document (frozen subset + applies cleanly), charges the per-identity
/// message rate limit (F2), then composes the body and gossips via
/// [`broadcast_state`]. No size check here — `Message::serialize` is the single
/// size gate, inside `broadcast_state`.
///
/// # Errors
/// Propagates a `broadcast_state` failure (oversize body / broadcast refusal).
pub(crate) async fn broadcast_state_patch(
    swarm: &SwarmId,
    author: &Nickname,
    patch: serde_json::Value,
    if_doc_hash: Option<String>,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
) -> anyhow::Result<StatePatchOutcome> {
    let current = crate::daemon::state_doc::derive_document(&state.state_log);
    // Optimistic-concurrency guard: reject if the document moved since the
    // caller's last read. The per-swarm event loop is single-threaded, so this
    // check and the insert below are atomic — no peer patch can interleave. A
    // stale write never reaches the wire, so this needs no fold-contract change.
    if let Some(expected) = &if_doc_hash {
        let actual = crate::daemon::state_doc::document_hash(&current);
        if *expected != actual {
            return Ok(StatePatchOutcome::Stale(format!(
                "stale document: --if-doc-hash {expected} no longer matches the current \
                 document ({actual}); re-read with `state get` and retry"
            )));
        }
    }
    if let Err(why) = crate::daemon::state_doc::validate_patch(&patch, &current) {
        return Ok(StatePatchOutcome::Invalid(why));
    }
    let pubkey = identity::encode_pubkey(&state.identity.public());
    if !state.rate_limiter.check(&pubkey) {
        return Ok(StatePatchOutcome::RateLimited);
    }
    let body = crate::daemon::state_doc::patch_body(patch)?;
    broadcast_state(swarm, author, body, state, sender, output).await?;
    Ok(StatePatchOutcome::Applied)
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
    meta: &crate::protocol::peer_meta::PeerMeta,
) {
    broadcast_msg(
        sender,
        &Message::new_joined(swarm, author, meta).signed(identity),
    )
    .await;
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

/// Retain a just-built outbound message in the local log (pruning the
/// fork/DAG indexes on any eviction) and write the dev log. The operator
/// echo is the caller's responsibility — it differs by kind
/// (`print_message_ex` for `Msg`, `print_handover` for `Handover`).
/// Shared by [`commit_outbound`] (after chain stamping) and
/// [`echo_and_retain`] (no chain stamping).
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

/// Echo a just-built outbound handover leg to the operator and retain it.
/// Shared by [`broadcast_handover`]'s meshed and queued paths (no chain
/// stamping; handover is presence-like).
/// Echo a just-built outbound exchange leg to the operator and, for **content**
/// legs, retain it in the message log for anti-entropy. The `Progress` phase
/// is liveness plumbing — echoed (so the sender's own widget updates) but
/// never retained, mirroring its receive-side handling.
fn echo_and_retain_task(state: &mut EventLoopState, msg: &Message, out: &output::Output) {
    out.print_exchange(msg, true);
    let retain = matches!(
        &msg.kind,
        MessageKind::Exchange { phase, .. } if crate::protocol::message::is_content_phase(*phase)
    );
    if retain {
        retain_outbound(state, msg);
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
    retain_outbound(state, msg);
}

/// Outcome of a send through [`broadcast_message`]. `RateLimited` is a
/// *drop*, not an error (mirrors the receiver-side drop) — returned to
/// the caller so a programmatic sender (`ahsw msg`, MCP `send_message`)
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

/// One outbound exchange leg's payload (addressee + correlation id + behavior +
/// phase + body), bundled so [`broadcast_exchange`] stays within the argument
/// budget. The IPC `exchange` command and the typed `SessionRequest::Exchange` both
/// build it from their carried fields.
pub(crate) struct ExchangeLeg {
    pub to: Nickname,
    pub exchange_id: ExchangeId,
    pub kind: ExchangeKind,
    pub phase: ExchangePhase,
    pub body: MessageBody,
}

/// Build, sign, log and gossip-broadcast one exchange leg. Sibling of
/// [`broadcast_message`] for the typed `Exchange` kind: **content** legs share the
/// per-author rate quota and the meshed/unmeshed retain paths, but the
/// `Progress` phase is liveness plumbing — rate-limit-exempt and never
/// retained. No hash-chain or DAG stamping (exchange legs are presence-like —
/// see [`MessageKind::Exchange`]). Always echoes the sender's own leg through
/// `print_exchange` (an `exchange`/`exchange_progress` event with `self:true`), the same
/// way an outbound `msg` echoes. Showing the leg to its *addressee* is the
/// receiver-side job of [`lifecycle::handle_exchange`](crate::lifecycle::handle_exchange).
///
/// This is where `Offer` addressee validation lives: an `Offer` leg must name
/// a current participant. The CLI, MCP, and embed callers all reach this
/// function, so validating here covers every path. Later phases skip the check
/// so a brief peer flap mid-exchange can't wedge the conversation.
///
/// # Errors
/// Returns an `unknown participant` error for an `Offer` to a non-participant;
/// propagates [`Message::serialize`] failure (oversized brief) and a gossip
/// broadcast error.
pub(crate) async fn broadcast_exchange(
    swarm: &SwarmId,
    author: &Nickname,
    leg: ExchangeLeg,
    state: &mut EventLoopState,
    sender: &GossipSender,
    out: &output::Output,
) -> anyhow::Result<SendOutcome> {
    let ExchangeLeg {
        to,
        exchange_id,
        kind,
        phase,
        body,
    } = leg;
    if matches!(phase, ExchangePhase::Offer) && !state.participants.contains(to.as_str()) {
        return Err(anyhow::anyhow!("unknown participant '{to}'"));
    }
    let signer = state.identity.clone();
    let our_pubkey = identity::encode_pubkey(&signer.public());
    // Content legs share the Msg quota; Progress (plumbing) is exempt.
    if crate::protocol::message::is_content_phase(phase) && !state.rate_limiter.check(&our_pubkey) {
        out.info(&format!("rate limit exceeded for [{author}], dropping"));
        tracing::debug!(%author, "send rate limit exceeded; not broadcasting exchange");
        return Ok(SendOutcome::RateLimited);
    }
    let msg =
        Message::new_exchange(swarm, author, to, exchange_id, kind, phase, body).signed(&signer);
    let bytes = Bytes::from(msg.serialize()?);
    let id = msg.id.clone();
    if state.meshed {
        // Meshed: echo + (content-only) retain locally, then hit the wire. A
        // transient broadcast error still leaves a content leg in our log for
        // anti-entropy.
        echo_and_retain_task(state, &msg, out);
        sender
            .broadcast(bytes)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
    } else if state.pending_outbound.push(bytes) {
        // Unmeshed: buffered for flush-on-connect; it *will* be sent in order.
        echo_and_retain_task(state, &msg, out);
    } else {
        out.info("pending outbound buffer full; outbound message dropped");
        tracing::warn!("pending outbound buffer full; outbound message dropped");
        return Ok(SendOutcome::RateLimited);
    }
    // Advance our own coarse state machine for the leg we just sent (resets
    // our debounce, advances the phase, counts content). Warn once if a
    // content leg pushed the exchange past its whole-exchange cap.
    if crate::daemon::exchange::ingest(&mut state.exchanges, &msg, true, Instant::now()) {
        out.info(&format!(
            "exchange exceeded {} messages; wrap it up",
            crate::util::consts::EXCHANGE_CONTENT_CAP
        ));
        tracing::warn!("exchange content cap exceeded");
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
        SessionRequest::Exchange {
            to,
            exchange_id,
            kind,
            phase,
            body,
            resp,
        } => {
            let leg = ExchangeLeg {
                to,
                exchange_id,
                kind,
                phase,
                body,
            };
            let outcome = broadcast_exchange(swarm, author, leg, state, sender, output)
                .await
                .map(|sent| match sent {
                    SendOutcome::Sent(_id, msg) => Some(msg),
                    SendOutcome::RateLimited => None,
                });
            let sent_ok = matches!(outcome, Ok(Some(_)));
            let _ = resp.send(outcome);
            sent_ok
        }
        SessionRequest::Peers { resp } => {
            let _ = resp.send(state.roster_snapshot());
            false
        }
        SessionRequest::AppendState { body, resp } => {
            let outcome = broadcast_state(swarm, author, body, state, sender, output).await;
            let sent_ok = outcome.is_ok();
            let _ = resp.send(outcome);
            sent_ok
        }
        SessionRequest::StateSnapshot { resp } => {
            let _ = resp.send(state.state_log.replay_bodies());
            false
        }
        SessionRequest::StatePatch {
            patch,
            if_doc_hash,
            resp,
        } => {
            let outcome =
                broadcast_state_patch(swarm, author, patch, if_doc_hash, state, sender, output)
                    .await;
            let sent = matches!(&outcome, Ok(StatePatchOutcome::Applied));
            let _ = resp.send(state_patch_reply(outcome));
            sent
        }
        SessionRequest::StateDocument { resp } => {
            let _ = resp.send(crate::daemon::state_doc::derive_document(&state.state_log));
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
