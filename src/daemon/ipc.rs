//! The IPC command application layer: turns a parsed `msg` / `poll`
//! command (decoded by the `transport::ipc` socket server) into a
//! gossip broadcast or a poll-buffer read. Kept here (not in
//! `transport::ipc`) because it needs `EventLoopState` — transport
//! must not depend on daemon state.

use iroh_gossip::api::GossipSender;
use tokio::sync::oneshot;

use std::collections::HashMap;
use std::time::Duration;

use crate::daemon::state::{EventLoopState, PingRound, PollResponder};
use crate::output;
use crate::protocol::{Message, Nickname, SwarmId};
use crate::transport::ipc::{IpcCommand, json_ack, json_error, json_ok_msg, json_stale};
use crate::util::tuning::ping_window_secs;

use crate::gossip::{
    ExchangeLeg, StatePatchOutcome, broadcast_exchange, broadcast_message, broadcast_msg,
    broadcast_state_patch,
};

/// Returns `true` if the handler broadcast anything, so the caller
/// can refresh `last_sent_at` for heartbeat suppression.
#[expect(
    clippy::too_many_lines,
    reason = "a dispatch match with one arm per IpcCommand; the state/meta channel pair doubles \
              the patch/get arms but each is a thin delegate"
)]
pub(crate) async fn handle_ipc_command(
    cmd: IpcCommand,
    resp_tx: oneshot::Sender<String>,
    swarm: &SwarmId,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
) -> bool {
    match cmd {
        IpcCommand::Msg {
            swarm: _,
            body,
            reply,
        } => {
            tracing::debug!(addressed = reply.is_some(), "IPC msg command received");
            match broadcast_message(swarm, author, body, reply, state, sender, output).await {
                Ok((msg_id, msg)) => {
                    let _ = resp_tx.send(json_ok_msg(&msg_id, &msg));
                    true
                }
                Err(error) => {
                    let _ = resp_tx.send(json_error(&error.to_string()));
                    false
                }
            }
        }
        IpcCommand::Poll {
            swarm: _,
            after,
            wait_ms,
        } => {
            // Surfaced-events ring, seq-cursored. Each event renders to the
            // *same* JSON line the live `--output json` stream emits (via
            // `surfaced_event_json`), with its `seq` flattened in so the client
            // advances `--after`. `poll_or_register` responds now if events are
            // buffered, else (with `wait_ms`) parks a waiter the loop fulfills
            // on the next surfaced event or expires at the deadline. A parked
            // waiter broadcasts nothing → `false`.
            state.poll_or_register(
                after,
                wait_ms,
                tokio::time::Instant::now(),
                PollResponder::Json(resp_tx),
            );
            false
        }
        IpcCommand::Ping { swarm: _ } => {
            // Arm a fresh round (replacing any in flight) and broadcast
            // the probe. Pongs are collected by the gossip receive path;
            // the round's deadline drives the `ping_report` emission.
            let now = tokio::time::Instant::now();
            state.ping_round = Some(Box::new(PingRound {
                t1: now,
                deadline: now + Duration::from_secs(ping_window_secs()),
                pongs: HashMap::new(),
                // CLI/IPC consumes the `ping_report` event, not a channel.
                resp: None,
            }));
            broadcast_msg(
                sender,
                &Message::new_ping(swarm, author).signed(&state.identity),
            )
            .await;
            tracing::debug!("IPC ping command received; round armed");
            let _ = resp_tx.send(json_ack());
            true
        }
        IpcCommand::Exchange {
            swarm: _,
            to,
            exchange_id,
            kind,
            phase,
            body,
        } => {
            // `broadcast_exchange` validates the addressee (Offer only); an
            // unknown participant comes back through the `Err` arm below as
            // `{"ok":false,"error":"unknown participant '<nick>'"}`.
            tracing::debug!(%to, %exchange_id, %kind, %phase, "IPC exchange command received");
            let leg = ExchangeLeg {
                to,
                exchange_id,
                kind,
                phase,
                body,
            };
            match broadcast_exchange(swarm, author, leg, state, sender, output).await {
                Ok((msg_id, msg)) => {
                    let _ = resp_tx.send(json_ok_msg(&msg_id, &msg));
                    true
                }
                Err(error) => {
                    let _ = resp_tx.send(json_error(&error.to_string()));
                    false
                }
            }
        }
        IpcCommand::Peers { swarm: _ } => {
            let _ = resp_tx.send(peers_response(state));
            false
        }
        IpcCommand::StatePatch {
            swarm: _,
            patch,
            if_doc_hash,
        } => {
            let outcome = broadcast_state_patch(
                swarm,
                author,
                patch,
                if_doc_hash,
                state,
                sender,
                output,
                crate::protocol::Channel::State,
            )
            .await;
            let (response, broadcast) = state_patch_response(outcome);
            let _ = resp_tx.send(response);
            broadcast
        }
        IpcCommand::StateGet { swarm: _ } => {
            let _ = resp_tx.send(state_get_response(state, crate::protocol::Channel::State));
            false
        }
        IpcCommand::MetaPatch {
            swarm: _,
            patch,
            if_doc_hash,
        } => {
            let outcome = broadcast_state_patch(
                swarm,
                author,
                patch,
                if_doc_hash,
                state,
                sender,
                output,
                crate::protocol::Channel::Meta,
            )
            .await;
            let (response, broadcast) = state_patch_response(outcome);
            let _ = resp_tx.send(response);
            broadcast
        }
        IpcCommand::MetaGet { swarm: _ } => {
            let _ = resp_tx.send(state_get_response(state, crate::protocol::Channel::Meta));
            false
        }
    }
}

/// Map a state-patch outcome to its IPC response JSON and whether anything was
/// broadcast (so the caller can refresh the heartbeat clock). A `Stale` conflict
/// gets the structured `stale` marker; only `Applied` counts as a broadcast.
fn state_patch_response(outcome: anyhow::Result<StatePatchOutcome>) -> (String, bool) {
    match outcome {
        Ok(StatePatchOutcome::Applied) => (json_ack(), true),
        Ok(StatePatchOutcome::Invalid(why)) => (json_error(&why), false),
        Ok(StatePatchOutcome::Stale(why)) => (json_stale(&why), false),
        Err(error) => (json_error(&error.to_string()), false),
    }
}

/// The `ahsw state get` response: the derived document plus its `doc_hash`
/// (the compare-and-set token a later `state patch --if-doc-hash` passes back).
fn state_get_response(state: &EventLoopState, channel: crate::protocol::Channel) -> String {
    let log = match channel {
        crate::protocol::Channel::State => &state.state_log,
        crate::protocol::Channel::Meta => &state.meta_log,
    };
    let document = crate::daemon::state_doc::derive_document(log);
    // Serialize the document once and reuse those exact bytes for both the
    // content hash (the CAS token) and the response body. `document_hash` hashes
    // the same `serde_json` encoding, so the hash is unchanged; splicing the
    // serialized JSON and the hex hash verbatim is safe (both are inert text).
    let doc_json = serde_json::to_string(&document).unwrap_or_else(|_| "null".to_owned());
    let doc_hash = crate::protocol::identity::content_hash_hex(doc_json.as_bytes());
    format!(r#"{{"ok":true,"document":{doc_json},"doc_hash":"{doc_hash}"}}"#)
}

/// Serialize the live roster snapshot as the `ahsw peers` response.
/// `ok:true` plus the snapshot's `participants` (recency-sorted, peers only)
/// and `participant_count` (`participants.len() + 1` — the `+1` is self, so
/// the count is swarm size, not the array length). Matches the field name the
/// MCP `swarm_info` result and the state file already use for this quantity.
fn peers_response(state: &EventLoopState) -> String {
    let snapshot = state.roster_snapshot();
    serde_json::json!({
        "ok": true,
        "participants": snapshot.participants,
        "participant_count": snapshot.count,
    })
    .to_string()
}
