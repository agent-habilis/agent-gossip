//! The **gossip request/response** server gate: the safe subset of A2A
//! operations a peer serves to other peers over gossip (`A2aReq` → `A2aResp`).
//!
//! Unlike the localhost JSON-RPC binding (`--a2a-serve`), which is
//! bearer-authenticated and exposes the full [`handle_op`](super::rpc::handle_op)
//! surface to a trusted local client, a gossip request is only
//! swarm-member-signed. So the gate here refuses anything that would make the
//! **serving** peer author state under its own identity on the caller's behalf
//! (identity laundering): no `swarm/state.merge`, no `swarm/meta.merge`, no
//! **broadcast** `message/send`. It serves only reads, a party-checked
//! `tasks/cancel`, and a `message/send` **directed at the serving peer** (the
//! caller asks the peer to take work; the peer ingests it and answers).

use serde_json::Value;

use crate::daemon::state::EventLoopState;
use crate::protocol::{Channel, Nickname};

use super::TaskId;
use super::rpc::{A2aOp, RpcError};

/// What a classified gossip request resolves to. Kept separate from
/// execution so the safe-set decision is a pure, unit-testable function; the
/// receive path (`gossip::recv`) runs the resulting action.
pub(crate) enum Served {
    /// A read (or party-checked cancel) to run through [`handle_op`].
    Op(A2aOp),
    /// A `message/send` **to us** — the payload is ingested locally (a task
    /// offer opens a task; chat surfaces) and the resulting task/ack returned.
    /// The peer is the terminal recipient and never re-broadcasts.
    Ingest(super::Message),
    /// A `shard/repair` ask: re-deliver the named cached shard frames of one
    /// of our big outbound groups (see `daemon::reassembly::ShardCache`).
    ShardRepair {
        group: crate::protocol::ShardGroup,
        missing: Vec<u32>,
    },
    /// Not permitted over gossip (unknown method, or one that would author on
    /// the caller's behalf).
    Reject(RpcError),
}

/// Classify one gossip A2A request (`method` + `params`, from `requester`)
/// into the safe action to run. Pure — reads `state` only for the
/// `tasks/cancel` party check.
pub(crate) fn classify(
    method: &str,
    params: &Value,
    requester: &Nickname,
    state: &EventLoopState,
) -> Served {
    let task_id = || {
        params["id"]
            .as_str()
            .and_then(TaskId::from_uuid_str)
            .ok_or_else(|| RpcError::invalid_params("params.id must be a task id (uuid)"))
    };
    match method {
        // `SubscribeToTask` returns the same snapshot as `GetTask`; the worker's
        // status/artifact frames already flood + heal to the task's parties (the
        // push plane IS the stream), so the caller keeps receiving the pushed
        // frames after the snapshot — connectionless, no held socket.
        "GetTask" | "SubscribeToTask" => match task_id() {
            Ok(task_id) => Served::Op(A2aOp::GetTask { task_id }),
            Err(error) => Served::Reject(error),
        },
        "ListTasks" => Served::Op(A2aOp::ListTasks),
        "swarm/state.get" => Served::Op(A2aOp::ChannelGet {
            channel: Channel::State,
        }),
        "swarm/meta.get" => Served::Op(A2aOp::ChannelGet {
            channel: Channel::Meta,
        }),
        "CancelTask" => match task_id() {
            Ok(task_id) => {
                // Party check: only a task's own peer may cancel it, so the
                // caller can't cancel work it isn't part of.
                let is_party = state
                    .tasks
                    .get(&task_id)
                    .is_some_and(|rec| rec.peer == *requester);
                if is_party {
                    Served::Op(A2aOp::CancelTask { task_id })
                } else {
                    Served::Reject(RpcError::invalid_params(
                        "not permitted: cancel is only allowed by the task's peer",
                    ))
                }
            }
            Err(error) => Served::Reject(error),
        },
        // Streaming create: identical to SendMessage (open the task); the
        // initiator then receives the worker's pushed status/artifact frames as
        // the stream. No held connection over gossip.
        "SendMessage" | "SendStreamingMessage" => {
            let message: super::Message = match serde_json::from_value(params["message"].clone()) {
                Ok(message) => message,
                Err(error) => {
                    return Served::Reject(RpcError::invalid_params(format!(
                        "params.message is not an A2A Message: {error}"
                    )));
                }
            };
            // A gossip peer is the terminal recipient — it serves only a
            // message directed at itself, never a broadcast (which would make
            // it re-author under its own identity for the caller).
            if message
                .extensions
                .iter()
                .any(|uri| uri == super::EXT_SWARM_BROADCAST)
            {
                return Served::Reject(RpcError::invalid_params(
                    "not permitted: broadcast message/send over gossip",
                ));
            }
            Served::Ingest(message)
        }
        // A receiver of one of our big (unlogged) multipart bodies asking for
        // the shards it lost — served from the outbound shard cache, which
        // only ever holds frames we authored and signed ourselves, so there
        // is nothing to launder. The addressee gate on directed frames is
        // enforced at the resend site.
        "shard/repair" => {
            let group = params["group"]
                .as_str()
                .and_then(crate::protocol::ShardGroup::from_uuid_str);
            let missing: Vec<u32> = params["missing"]
                .as_array()
                .map(|idxs| {
                    idxs.iter()
                        .filter_map(|idx| idx.as_u64().and_then(|idx| u32::try_from(idx).ok()))
                        .take(crate::util::consts::REASSEMBLY_REPAIR_MAX_IDXS)
                        .collect()
                })
                .unwrap_or_default();
            match group {
                Some(group) if !missing.is_empty() => Served::ShardRepair { group, missing },
                _ => Served::Reject(RpcError::invalid_params(
                    "shard/repair needs params.group (uuid) and a non-empty params.missing",
                )),
            }
        }
        // Authoring global state on the caller's behalf, or anything else.
        "swarm/state.merge" | "swarm/meta.merge" => Served::Reject(RpcError::not_permitted(method)),
        other => Served::Reject(RpcError::method_not_found(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::{Served, classify};
    use crate::daemon::state::EventLoopState;
    use crate::protocol::Nickname;
    use serde_json::json;

    fn state() -> EventLoopState {
        EventLoopState::new(
            None,
            std::time::Instant::now(),
            std::sync::Arc::new(crate::protocol::identity::Identity::generate()),
        )
    }

    fn is_reject(served: &Served) -> bool {
        matches!(served, Served::Reject(_))
    }

    #[test]
    fn reads_are_served() {
        let caller = Nickname::from("alice");
        let st = state();
        assert!(matches!(
            classify("ListTasks", &json!({}), &caller, &st),
            Served::Op(_)
        ));
        assert!(matches!(
            classify(
                "GetTask",
                &json!({"id": "550e8400-e29b-41d4-a716-446655440000"}),
                &caller,
                &st
            ),
            Served::Op(_)
        ));
        assert!(matches!(
            classify("swarm/state.get", &json!({}), &caller, &st),
            Served::Op(_)
        ));
        // Streaming ops are served: SubscribeToTask returns a snapshot op,
        // SendStreamingMessage ingests like a create.
        assert!(matches!(
            classify(
                "SubscribeToTask",
                &json!({"id": "550e8400-e29b-41d4-a716-446655440000"}),
                &caller,
                &st
            ),
            Served::Op(_)
        ));
        assert!(matches!(
            classify(
                "SendStreamingMessage",
                &json!({"message": {
                    "messageId": "550e8400-e29b-41d4-a716-446655440000",
                    "role": "ROLE_USER",
                    "parts": [{"text": "stream it"}],
                }}),
                &caller,
                &st
            ),
            Served::Ingest(_)
        ));
    }

    #[test]
    fn merges_and_broadcast_send_are_rejected() {
        let caller = Nickname::from("alice");
        let st = state();
        assert!(is_reject(&classify(
            "swarm/state.merge",
            &json!({"merge": {"x": 1}}),
            &caller,
            &st
        )));
        assert!(is_reject(&classify(
            "swarm/meta.merge",
            &json!({"merge": {"x": 1}}),
            &caller,
            &st
        )));
        // A broadcast message/send (declares the swarm-broadcast extension) is
        // refused — the peer must not re-author it under its own identity.
        let broadcast = json!({"message": {
            "messageId": "550e8400-e29b-41d4-a716-446655440000",
            "role": "ROLE_USER",
            "parts": [{"text": "hi"}],
            "extensions": [super::super::EXT_SWARM_BROADCAST],
        }});
        assert!(is_reject(&classify(
            "SendMessage",
            &broadcast,
            &caller,
            &st
        )));
    }

    #[test]
    fn directed_send_is_ingested() {
        let caller = Nickname::from("alice");
        let st = state();
        let directed = json!({"message": {
            "messageId": "550e8400-e29b-41d4-a716-446655440000",
            "role": "ROLE_USER",
            "parts": [{"text": "review src/net"}],
        }});
        assert!(matches!(
            classify("SendMessage", &directed, &caller, &st),
            Served::Ingest(_)
        ));
    }

    #[test]
    fn cancel_requires_being_the_task_peer() {
        let caller = Nickname::from("alice");
        let st = state();
        // No such task → rejected (not a party).
        assert!(is_reject(&classify(
            "CancelTask",
            &json!({"id": "550e8400-e29b-41d4-a716-446655440000"}),
            &caller,
            &st
        )));
    }

    #[test]
    fn shard_repair_is_classified_and_validated() {
        let caller = Nickname::from("alice");
        let st = state();
        assert!(matches!(
            classify(
                "shard/repair",
                &json!({"group": "550e8400-e29b-41d4-a716-446655440000", "missing": [0, 3, 7]}),
                &caller,
                &st
            ),
            Served::ShardRepair { missing, .. } if missing == vec![0, 3, 7]
        ));
        // A non-uuid group or an empty missing list is malformed.
        assert!(is_reject(&classify(
            "shard/repair",
            &json!({"group": "nope", "missing": [0]}),
            &caller,
            &st
        )));
        assert!(is_reject(&classify(
            "shard/repair",
            &json!({"group": "550e8400-e29b-41d4-a716-446655440000", "missing": []}),
            &caller,
            &st
        )));
    }

    #[test]
    fn unknown_method_is_rejected() {
        let caller = Nickname::from("alice");
        let st = state();
        assert!(is_reject(&classify(
            "message/stream",
            &json!({}),
            &caller,
            &st
        )));
    }
}
