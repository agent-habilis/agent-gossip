//! The IPC command application layer: turns a parsed `msg` / `poll`
//! command (decoded by the `transport::ipc` socket server) into a
//! gossip broadcast or a poll-buffer read. Kept here (not in
//! `transport::ipc`) because it needs `EventLoopState` — transport
//! must not depend on daemon state.

use iroh_gossip::api::GossipSender;
use tokio::sync::oneshot;

use std::collections::HashMap;
use std::time::Duration;

use crate::daemon::state::{EventLoopState, PingRound};
use crate::output;
use crate::protocol::{Message, Nickname, SwarmId};
use crate::transport::ipc::{IpcCommand, json_ack, json_error, json_ok_msg, json_rate_limited};
use crate::util::tuning::ping_window_secs;

use crate::gossip::{
    ExchangeLeg, SendOutcome, broadcast_exchange, broadcast_message, broadcast_msg,
};

/// Returns `true` if the handler broadcast anything, so the caller
/// can refresh `last_sent_at` for heartbeat suppression.
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
                Ok(SendOutcome::Sent(msg_id, msg)) => {
                    let _ = resp_tx.send(json_ok_msg(&msg_id, &msg));
                    true
                }
                Ok(SendOutcome::RateLimited) => {
                    let _ = resp_tx.send(json_rate_limited());
                    false
                }
                Err(error) => {
                    let _ = resp_tx.send(json_error(&error.to_string()));
                    false
                }
            }
        }
        IpcCommand::Poll { swarm: _, after } => {
            // Shared with the typed in-process `Poll` (join-horizon
            // filtered); the CLI socket just serializes the result.
            let messages = state.poll_after(after.as_ref(), output);
            let resp = serde_json::to_string(&messages)
                .expect("serializing poll response (Vec<Message>) is infallible");
            let _ = resp_tx.send(resp);
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
                Ok(SendOutcome::Sent(msg_id, msg)) => {
                    let _ = resp_tx.send(json_ok_msg(&msg_id, &msg));
                    true
                }
                Ok(SendOutcome::RateLimited) => {
                    let _ = resp_tx.send(json_rate_limited());
                    false
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
    }
}

/// Serialize the live roster snapshot as the `ahs peers` response.
/// `ok:true` plus the snapshot's `participants` (recency-sorted) and
/// `count` (`participants.len() + 1`).
fn peers_response(state: &EventLoopState) -> String {
    let snapshot = state.roster_snapshot();
    serde_json::json!({
        "ok": true,
        "participants": snapshot.participants,
        "count": snapshot.count,
    })
    .to_string()
}
