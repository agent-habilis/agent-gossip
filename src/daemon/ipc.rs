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
use crate::protocol::swarm::SwarmName;
use crate::protocol::{Message, Nickname, SwarmId};
use crate::transport::ipc::{IpcCommand, json_ack, json_error, json_ok_msg};
use crate::util::tuning::ping_window_secs;

use crate::gossip::{
    TaskLeg, broadcast_message, broadcast_msg, broadcast_state_merge, broadcast_task,
};

/// Returns `true` if the handler broadcast anything, so the caller
/// can refresh `last_sent_at` for heartbeat suppression.
#[expect(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "a dispatch match with one arm per IpcCommand (the state/meta channel pair \
              doubles the patch/get arms), plus the daemon's own identity (swarm/name/author), \
              the live state, gossip sender, and output sink"
)]
pub(crate) async fn handle_ipc_command(
    cmd: IpcCommand,
    resp_tx: oneshot::Sender<String>,
    swarm: &SwarmId,
    name: &SwarmName,
    author: &Nickname,
    state: &mut EventLoopState,
    sender: &GossipSender,
    output: &output::Output,
) -> bool {
    // The per-swarm socket path already routes a command to the right daemon,
    // but a command carries its own swarm id — validate it matches ours rather
    // than binding it to `_` and trusting the path alone (a stale socket path or
    // a symlinked runtime dir would otherwise misroute a signed broadcast). The
    // `Info` probe carries no swarm and is addressed purely by socket path.
    if let Some(cmd_swarm) = cmd.swarm_id()
        && cmd_swarm != swarm
    {
        let _ = resp_tx.send(json_error("command swarm id does not match this daemon"));
        return false;
    }
    match cmd {
        IpcCommand::Msg {
            swarm: _,
            body,
            reply,
        } => {
            tracing::debug!(addressed = reply.is_some(), "IPC msg command received");
            let kind = crate::protocol::MessageKind::Msg { reply };
            match broadcast_message(swarm, author, body, kind, state, sender, output).await {
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
        IpcCommand::Notice {
            swarm: _,
            body,
            reply,
        } => {
            tracing::debug!(addressed = reply.is_some(), "IPC notice command received");
            let kind = crate::protocol::MessageKind::Notice { reply };
            match broadcast_message(swarm, author, body, kind, state, sender, output).await {
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
            long,
        } => {
            // Surfaced-events ring, seq-cursored. Each event renders to the
            // *same* JSON line the live `--output json` stream emits (via
            // `surfaced_event_json`), with its `seq` flattened in so the client
            // advances `--after`. `poll_or_register` responds now if events are
            // buffered, else (with `long`) parks a waiter the loop fulfills
            // on the next surfaced event or expires at the park cap. A parked
            // waiter broadcasts nothing → `false`.
            state.poll_or_register(
                after,
                long,
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
        IpcCommand::Task {
            swarm: _,
            to,
            task_id,
            phase,
            body,
        } => {
            // `broadcast_task` validates the addressee (Offer only); an
            // unknown participant comes back through the `Err` arm below as
            // `{"ok":false,"error":"unknown participant '<nick>'"}`.
            tracing::debug!(%to, %task_id, %phase, "IPC task command received");
            let leg = TaskLeg {
                to,
                task_id,
                phase,
                body,
            };
            match broadcast_task(swarm, author, leg, state, sender, output).await {
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
        IpcCommand::StateMerge { swarm: _, merge } => {
            let outcome = broadcast_state_merge(
                swarm,
                author,
                merge,
                state,
                sender,
                output,
                crate::protocol::Channel::State,
            )
            .await;
            let (response, broadcast) = state_merge_response(outcome);
            let _ = resp_tx.send(response);
            broadcast
        }
        IpcCommand::StateGet { swarm: _ } => {
            let _ = resp_tx.send(state_get_response(state, crate::protocol::Channel::State));
            false
        }
        IpcCommand::MetaMerge { swarm: _, merge } => {
            let outcome = broadcast_state_merge(
                swarm,
                author,
                merge,
                state,
                sender,
                output,
                crate::protocol::Channel::Meta,
            )
            .await;
            let (response, broadcast) = state_merge_response(outcome);
            let _ = resp_tx.send(response);
            broadcast
        }
        IpcCommand::MetaGet { swarm: _ } => {
            let _ = resp_tx.send(state_get_response(state, crate::protocol::Channel::Meta));
            false
        }
        IpcCommand::Info => {
            let _ = resp_tx.send(info_response(swarm, name, author, state));
            false
        }
    }
}

/// The `doctor` identity probe response: this daemon's own swarm id, human
/// name, nickname, and swarm size. `participant_count` matches the field name
/// `peers` / the state file / MCP `swarm_info` already use for swarm size
/// (roster peers + 1 for self).
fn info_response(
    swarm: &SwarmId,
    name: &SwarmName,
    author: &Nickname,
    state: &EventLoopState,
) -> String {
    serde_json::json!({
        "ok": true,
        "swarm": swarm.as_str(),
        "name": name.as_str(),
        "nickname": author.as_str(),
        "participant_count": state.roster_snapshot().count,
    })
    .to_string()
}

/// Map a state-merge outcome to its IPC response JSON and whether anything was
/// broadcast (so the caller can refresh the heartbeat clock). A merge always
/// applies, so `Ok` is the broadcast/ack and `Err` is a transport failure.
fn state_merge_response(outcome: anyhow::Result<()>) -> (String, bool) {
    match outcome {
        Ok(()) => (json_ack(), true),
        Err(error) => (json_error(&error.to_string()), false),
    }
}

/// The `agent-gossip state get` response: the derived document.
fn state_get_response(state: &EventLoopState, channel: crate::protocol::Channel) -> String {
    let log = match channel {
        crate::protocol::Channel::State => &state.state_log,
        crate::protocol::Channel::Meta => &state.meta_log,
    };
    let document = crate::daemon::state_doc::derive_document(log);
    let doc_json = serde_json::to_string(&document).unwrap_or_else(|_| "null".to_owned());
    format!(r#"{{"ok":true,"document":{doc_json}}}"#)
}

/// Serialize the live roster snapshot as the `agent-gossip peers` response.
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
