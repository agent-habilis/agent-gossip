use anyhow::Result;
use habilis_network::protocol::MeshId;

use super::a2a_call_sugar::A2aCallSugar;
use crate::a2a::status_update_params;

/// An `a2a call` request typed by method, so a task method cannot exist
/// without its task id.
pub(super) enum A2aCallRequest<'caller> {
    SendMessage {
        mesh: &'caller MeshId,
        task_id: Option<&'caller crate::a2a::TaskId>,
        text: &'caller str,
        label: Option<&'caller str>,
    },
    TaskById {
        task_id: &'caller crate::a2a::TaskId,
    },
    /// A method the CLI has no sugar for; `--params` is its only input.
    Unsugared,
}

impl<'a> A2aCallRequest<'a> {
    pub(super) fn parse(method: &str, sugar: &A2aCallSugar<'a>) -> Result<Self> {
        let A2aCallSugar {
            mesh,
            text,
            task_id,
            label,
        } = *sugar;
        match method {
            "SendMessage" | "SendStreamingMessage" => Ok(Self::SendMessage {
                mesh,
                task_id,
                text: text.unwrap_or_default(),
                label,
            }),
            "GetTask" | "CancelTask" | "SubscribeToTask" => task_id
                .map(|task_id| Self::TaskById { task_id })
                .ok_or_else(|| anyhow::anyhow!("--method {method} requires --task-id")),
            _ => Ok(Self::Unsugared),
        }
    }

    pub(super) fn params(&self) -> serde_json::Value {
        match *self {
            Self::SendMessage {
                mesh,
                task_id,
                text,
                label,
            } => {
                let message =
                    status_update_params::send_message_payload(mesh, task_id, text, label);
                serde_json::json!({ "message": message })
            }
            Self::TaskById { task_id } => serde_json::json!({ "id": task_id }),
            Self::Unsugared => serde_json::json!({}),
        }
    }
}
