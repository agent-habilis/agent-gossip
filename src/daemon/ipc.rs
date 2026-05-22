//! The IPC command application layer: turns a parsed `msg` / `poll`
//! command (decoded by the `transport::ipc` socket server) into a
//! gossip broadcast or a poll-buffer read. Kept here (not in
//! `transport::ipc`) because it needs `EventLoopState` — transport
//! must not depend on daemon state.

use iroh_gossip::api::GossipSender;
use tokio::sync::oneshot;

use crate::daemon::state::EventLoopState;
use crate::output;
use crate::protocol::{Nickname, SwarmId};
use crate::transport::ipc::{IpcCommand, json_error, json_ok_msg, json_rate_limited};

use crate::gossip::{SendOutcome, broadcast_message};

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
            let (mut messages, evicted) = state.message_log.messages_after(after.as_ref());
            if evicted {
                output.info("poll: --after ID was evicted from buffer, returning all messages");
            }
            // Join horizon: the log keeps pre-join messages (for
            // anti-entropy), but `poll`/`fetch` never surface them.
            messages.retain(|message| message.timestamp >= state.joined_at);
            tracing::debug!(
                returned = messages.len(),
                evicted,
                "IPC poll command served"
            );
            let resp = serde_json::to_string(&messages)
                .expect("serializing poll response (Vec<Message>) is infallible");
            let _ = resp_tx.send(resp);
            false
        }
    }
}
