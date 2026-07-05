use agent_habilis_gossip::transport::SwarmSender;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use std::collections::HashMap;
use std::time::Duration;

use crate::a2a::app::A2aApp;
use crate::a2a::surfaced::PollResponder;
use crate::a2a::{TaskId, TaskState};
use crate::output;
use agent_habilis_gossip::daemon::state::{EventLoopState, PingRound};
use agent_habilis_gossip::protocol::swarm::SwarmName;
use agent_habilis_gossip::protocol::{Message, MessageBody, Nickname, SwarmId};
use agent_habilis_gossip::transport::ipc::{Addressed, json_ack, json_error, json_ok_msg};
use agent_habilis_gossip::util::tuning::ping_window_secs;

use crate::a2a::send::{broadcast_message, emit_task_artifact, emit_task_status};
use agent_habilis_gossip::gossip::{broadcast_msg, broadcast_state_merge};

/// Command sent from CLI to the running server over IPC. App-side because its
/// arms carry a2a-typed payloads (task id/state, gossip A2A calls); the engine's
/// `transport::ipc` socket server is generic over this type and names none of it.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command")]
pub(crate) enum IpcCommand {
    /// Broadcast a swarm chat message (A2A `message/send` with no addressee).
    #[serde(rename = "msg")]
    Msg { swarm: SwarmId, body: MessageBody },
    #[serde(rename = "poll")]
    Poll {
        swarm: SwarmId,
        /// Surfaced-event seq cursor: return events surfaced after this seq.
        /// Omitted on the first poll (returns the buffered history). The
        /// per-event `seq` in the response is the value to pass next.
        #[serde(skip_serializing_if = "Option::is_none")]
        after: Option<u64>,
        /// Long-poll: park this read up to the server cap (`longpoll_max_ms`),
        /// returning early on the first new surfaced event, else `[]` at the
        /// deadline. Absent/false is an immediate read. Skipped when false so
        /// the wire stays byte-stable for callers that never long-poll.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        long: bool,
    },
    /// Arm an RTT round: the daemon broadcasts a ping probe, collects
    /// pongs for a fixed window, and emits a `ping_report` on its
    /// `--output json` stream. Fire-and-forget — the ack is immediate.
    #[serde(rename = "ping")]
    Ping { swarm: SwarmId },
    /// Worker-emit a `TaskStatusUpdate` on a task we're serving (`a2a status`).
    #[serde(rename = "a2a_status")]
    A2aStatus {
        swarm: SwarmId,
        task_id: TaskId,
        #[serde(with = "crate::a2a::friendly_state")]
        state: TaskState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Worker-emit a `TaskArtifactUpdate` (the result) on a task (`a2a artifact`).
    /// An optional `file` is offloaded over the blob channel and referenced as a
    /// `Part.url`; a path (not bytes) so the IPC line stays bounded.
    #[serde(rename = "a2a_artifact")]
    A2aArtifact {
        swarm: SwarmId,
        task_id: TaskId,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<std::path::PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_mime: Option<String>,
    },
    /// Query the live participant roster (nicknames + recency) — backs the
    /// task sender's target picker and nickname validation.
    #[serde(rename = "peers")]
    Peers { swarm: SwarmId },
    /// Apply an RFC 7386 JSON Merge Patch to the swarm's shared state. `merge` is
    /// any JSON value merged into the document (an object deep-merges, `null`
    /// deletes a key, a non-object replaces the target); the daemon signs +
    /// gossips it.
    #[serde(rename = "state_merge")]
    StateMerge {
        swarm: SwarmId,
        merge: serde_json::Value,
    },
    /// Read the current derived shared-state document.
    #[serde(rename = "state_get")]
    StateGet { swarm: SwarmId },
    /// `meta`-channel counterpart of [`StateMerge`](IpcCommand::StateMerge).
    #[serde(rename = "meta_merge")]
    MetaMerge {
        swarm: SwarmId,
        merge: serde_json::Value,
    },
    /// `meta`-channel counterpart of [`StateGet`](IpcCommand::StateGet).
    #[serde(rename = "meta_get")]
    MetaGet { swarm: SwarmId },
    /// The relay routing topology from this daemon's point of view: the
    /// metric-weighted mesh graph it has assembled from gossiped link-state.
    #[serde(rename = "topology")]
    Topology { swarm: SwarmId },
    /// A gossip A2A call: send a directed A2A JSON-RPC request to `to` and
    /// return its response (or a timeout error). `to` serves the safe method
    /// set only.
    #[serde(rename = "a2a_call")]
    A2aCall {
        swarm: SwarmId,
        to: Nickname,
        method: String,
        #[serde(default)]
        params: serde_json::Value,
        timeout_secs: u64,
    },
    /// Identity probe for `doctor`: the daemon answers with its own swarm id,
    /// human name, nickname, and participant count. Carries no swarm — a
    /// socket serves exactly one swarm and `doctor` is asking *which*, so it
    /// addresses the daemon by socket path, not by id.
    #[serde(rename = "info")]
    Info,
}

impl Addressed for IpcCommand {
    /// The swarm a command is addressed to, used to derive the socket path in
    /// [`agent_habilis_gossip::transport::ipc::send`]. `None` for [`IpcCommand::Info`], which is
    /// sent by socket path directly (the daemon answers with its own id).
    fn swarm_id(&self) -> Option<&SwarmId> {
        match self {
            IpcCommand::Msg { swarm, .. }
            | IpcCommand::Poll { swarm, .. }
            | IpcCommand::Ping { swarm }
            | IpcCommand::A2aStatus { swarm, .. }
            | IpcCommand::A2aArtifact { swarm, .. }
            | IpcCommand::Peers { swarm }
            | IpcCommand::StateMerge { swarm, .. }
            | IpcCommand::StateGet { swarm }
            | IpcCommand::MetaMerge { swarm, .. }
            | IpcCommand::MetaGet { swarm }
            | IpcCommand::Topology { swarm }
            | IpcCommand::A2aCall { swarm, .. } => Some(swarm),
            IpcCommand::Info => None,
        }
    }
}

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
    app: &mut A2aApp,
    sender: &SwarmSender,
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
        IpcCommand::Msg { swarm: _, body } => {
            tracing::debug!("IPC msg command received");
            match broadcast_message(swarm, author, body, state, sender, output).await {
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
            app.surfaced.poll_or_register(
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
        IpcCommand::A2aStatus {
            swarm: _,
            task_id,
            state: task_state,
            note,
        } => {
            tracing::debug!(%task_id, ?task_state, "IPC a2a status command received");
            match emit_task_status(
                swarm,
                author,
                &task_id,
                task_state,
                note.as_deref(),
                state,
                app,
                sender,
                output,
            )
            .await
            {
                Ok(msg) => {
                    let _ = resp_tx.send(json_ok_msg(&msg.id.clone(), &msg));
                    true
                }
                Err(error) => {
                    let _ = resp_tx.send(json_error(&error.to_string()));
                    false
                }
            }
        }
        IpcCommand::A2aArtifact {
            swarm: _,
            task_id,
            text,
            file,
            file_name,
            file_mime,
        } => {
            tracing::debug!(%task_id, "IPC a2a artifact command received");
            let file = file.map(|path| agent_habilis_gossip::blob::FileRef {
                path,
                name: file_name,
                mime: file_mime,
            });
            match emit_task_artifact(
                swarm, author, &task_id, &text, file, state, app, sender, output,
            )
            .await
            {
                Ok(msg) => {
                    let _ = resp_tx.send(json_ok_msg(&msg.id.clone(), &msg));
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
                agent_habilis_gossip::protocol::Channel::State,
                true,
            )
            .await;
            let (response, broadcast) = state_merge_response(outcome);
            let _ = resp_tx.send(response);
            broadcast
        }
        IpcCommand::StateGet { swarm: _ } => {
            let _ = resp_tx.send(state_get_response(
                state,
                agent_habilis_gossip::protocol::Channel::State,
            ));
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
                agent_habilis_gossip::protocol::Channel::Meta,
                true,
            )
            .await;
            let (response, broadcast) = state_merge_response(outcome);
            let _ = resp_tx.send(response);
            broadcast
        }
        IpcCommand::MetaGet { swarm: _ } => {
            let _ = resp_tx.send(state_get_response(
                state,
                agent_habilis_gossip::protocol::Channel::Meta,
            ));
            false
        }
        IpcCommand::Topology { swarm: _ } => {
            let _ = resp_tx.send(topology_response(state));
            false
        }
        IpcCommand::A2aCall {
            swarm: _,
            to,
            method,
            params,
            timeout_secs,
        } => {
            // The response arrives later (the peer's `A2aResp`, or a timeout),
            // so park `resp_tx` in the waiter — like the long-poll `Poll` arm —
            // and answer nothing now.
            crate::a2a::send::broadcast_a2a_call(
                swarm,
                author,
                to,
                &method,
                params,
                Duration::from_secs(timeout_secs),
                crate::a2a::app::A2aResponder::Ipc(resp_tx),
                state,
                app,
                sender,
            )
            .await;
            true
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

/// Serialize this daemon's circuit routing topology (its assembled mesh graph) for
/// the `topology` IPC query. `{"ok":true,"topology":{self_id, edges:[…]}}`.
fn topology_response(state: &EventLoopState) -> String {
    let Some(endpoint) = state.unicast_pool.endpoint() else {
        return r#"{"ok":true,"topology":{"self_id":"","edges":[]}}"#.to_owned();
    };
    let topology = state.link_state.topology(endpoint.id());
    let topo_json = serde_json::to_string(&topology).unwrap_or_else(|_| "null".to_owned());
    format!(r#"{{"ok":true,"topology":{topo_json}}}"#)
}

/// The `agent-gossip state get` response: the derived document.
fn state_get_response(
    state: &EventLoopState,
    channel: agent_habilis_gossip::protocol::Channel,
) -> String {
    let document = match channel {
        agent_habilis_gossip::protocol::Channel::State => state.state_doc.to_json(),
        agent_habilis_gossip::protocol::Channel::Meta => state.meta_doc.to_json(),
    };
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

#[cfg(test)]
mod tests {
    use super::{IpcCommand, MessageBody, Nickname, SwarmId, TaskId, TaskState};
    use agent_habilis_gossip::transport::ipc::Addressed;

    // ── IpcCommand serialization ───────────────────────────────────
    //
    // The a2a-typed command set lives app-side; these guard its wire format
    // (the CLI IPC protocol) — the engine's generic socket server names none
    // of it.

    #[test]
    fn ipc_command_msg_round_trip() {
        let cmd = IpcCommand::Msg {
            swarm: SwarmId::from("💬://test"),
            body: MessageBody::from("hello"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.swarm_id().expect("Msg is swarm-addressed").as_str(),
            "💬://test"
        );
    }

    #[test]
    fn ipc_command_info_round_trip() {
        let json = serde_json::to_string(&IpcCommand::Info).unwrap();
        assert_eq!(json, r#"{"command":"info"}"#);
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        assert!(parsed.swarm_id().is_none(), "Info carries no swarm");
    }

    #[test]
    fn ipc_command_state_merge_round_trip() {
        let cmd = IpcCommand::StateMerge {
            swarm: SwarmId::from("💬://test"),
            merge: serde_json::json!({"turn": "b"}),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""command":"state_merge""#), "tag: {json}");
        assert!(json.contains(r#""merge""#), "{json}");
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::StateMerge { swarm, merge } => {
                assert_eq!(swarm.as_str(), "💬://test");
                assert_eq!(merge, serde_json::json!({"turn": "b"}));
            }
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Topology { .. }
            | IpcCommand::Info => panic!("expected StateMerge"),
        }
    }

    #[test]
    fn ipc_command_poll_round_trip() {
        let cmd = IpcCommand::Poll {
            swarm: SwarmId::from("💬://test"),
            after: Some(42),
            long: false,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        // `long: false` is skipped on the wire, keeping the format byte-stable
        // for callers that never long-poll.
        assert!(!json.contains("long"), "false long must not serialize");
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Poll { after, long, .. } => {
                assert_eq!(after, Some(42));
                assert!(!long, "absent long deserializes to false");
            }
            IpcCommand::Msg { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Topology { .. }
            | IpcCommand::Info => panic!("expected Poll"),
        }
    }

    #[test]
    fn ipc_command_ping_round_trip() {
        let cmd = IpcCommand::Ping {
            swarm: SwarmId::from("💬://test"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"ping\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Ping { swarm } => assert_eq!(swarm.as_str(), "💬://test"),
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Topology { .. }
            | IpcCommand::Info => panic!("expected Ping"),
        }
    }

    #[test]
    fn ipc_command_a2a_status_round_trip() {
        let cmd = IpcCommand::A2aStatus {
            swarm: SwarmId::from("💬://test"),
            task_id: TaskId::from("550e8400-e29b-41d4-a716-446655440000"),
            state: TaskState::Working,
            note: Some("on it".to_string()),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"a2a_status\""));
        assert!(json.contains("\"state\":\"working\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::A2aStatus { state, note, .. } => {
                assert_eq!(state, TaskState::Working);
                assert_eq!(note.as_deref(), Some("on it"));
            }
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::Peers { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Topology { .. }
            | IpcCommand::Info => panic!("expected A2aStatus"),
        }
    }

    #[test]
    fn ipc_command_peers_round_trip() {
        let cmd = IpcCommand::Peers {
            swarm: SwarmId::from("💬://test"),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"command\":\"peers\""));
        let parsed: IpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            IpcCommand::Peers { swarm } => assert_eq!(swarm.as_str(), "💬://test"),
            IpcCommand::Msg { .. }
            | IpcCommand::Poll { .. }
            | IpcCommand::Ping { .. }
            | IpcCommand::A2aStatus { .. }
            | IpcCommand::A2aArtifact { .. }
            | IpcCommand::StateMerge { .. }
            | IpcCommand::MetaMerge { .. }
            | IpcCommand::MetaGet { .. }
            | IpcCommand::StateGet { .. }
            | IpcCommand::A2aCall { .. }
            | IpcCommand::Topology { .. }
            | IpcCommand::Info => panic!("expected Peers"),
        }
    }

    #[test]
    fn ipc_command_poll_no_after_skips_field() {
        let cmd = IpcCommand::Poll {
            swarm: SwarmId::from("💬://test"),
            after: None,
            long: false,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(!json.contains("after"));
        assert!(!json.contains("long"));
    }

    #[test]
    fn ipc_command_poll_long_serializes_true() {
        let cmd = IpcCommand::Poll {
            swarm: SwarmId::from("💬://test"),
            after: None,
            long: true,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"long\":true"), "wire: {json}");
    }

    mod prop {
        use proptest::collection::vec as arb_vec;
        use proptest::{prop_assert, prop_assert_eq, proptest, strategy::Strategy};

        use super::{MessageBody, Nickname, SwarmId};

        fn arb_ascii_body() -> impl Strategy<Value = String> {
            arb_vec(0x20u8..0x7Eu8, 0..200).prop_map(|bytes| String::from_utf8(bytes).unwrap())
        }

        fn arb_nickname() -> impl Strategy<Value = String> {
            "[a-z]{3,8}-[a-z]{3,8}"
        }

        fn arb_swarm() -> impl Strategy<Value = SwarmId> {
            "💬[1-9A-HJ-NP-Za-km-z]{4,24}".prop_map(|raw| SwarmId::new(raw).unwrap())
        }

        proptest! {
            #![proptest_config(crate::proptest_support::config())]
            // ── Round-trip: build_msg_bytes -> Message::parse ──────

            #[test]
            fn prop_build_msg_bytes_message_round_trip(
                swarm in arb_swarm(),
                body in arb_ascii_body(),
                author in arb_nickname(),
            ) {
                let author = Nickname::new(author).unwrap();
                let body = MessageBody::new(body).unwrap();
                let expected_body = body.clone();
                let identity = agent_habilis_gossip::protocol::identity::Identity::generate();
                let (bytes, built) = agent_habilis_gossip::protocol::message::build_msg_bytes(
                    &swarm,
                    body,
                    &author,
                    &identity,
                    agent_habilis_gossip::protocol::message::ChainCtx::genesis(),
                )
                .unwrap();
                prop_assert!(!built.id.as_str().is_empty());
                let parsed = agent_habilis_gossip::protocol::Message::parse(&bytes).unwrap();
                prop_assert_eq!(&parsed.author, &author);
                prop_assert_eq!(&parsed.body, &expected_body);
                prop_assert_eq!(&parsed.swarm, &swarm);
                prop_assert_eq!(
                    parsed.kind,
                    agent_habilis_gossip::protocol::MessageKind::app_broadcast(crate::a2a::wire::MSG)
                );
            }
        }
    }
}
