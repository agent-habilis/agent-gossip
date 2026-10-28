use habilis_network::protocol::MeshId;
use serde_json::json;

use super::a2a_call_request::A2aCallRequest;
use super::a2a_call_sugar::A2aCallSugar;
use crate::a2a::TaskId;

const MESH: &str = "test";
const TASK: &str = "913f33fd-ef60-4dff-82f2-37bc1fd48bfe";

fn params(
    method: &str,
    text: Option<&str>,
    task_id: Option<&str>,
    label: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let mesh = MeshId::from(MESH);
    let task_id: Option<TaskId> = task_id.map(|raw| raw.parse().unwrap());
    let request = A2aCallRequest::parse(
        method,
        &A2aCallSugar {
            mesh: &mesh,
            text,
            task_id: task_id.as_ref(),
            label,
        },
    )?;
    Ok(request.params())
}

#[test]
fn a_task_method_with_a_task_id_sends_only_the_id() {
    for method in ["GetTask", "CancelTask", "SubscribeToTask"] {
        let sent = params(method, None, Some(TASK), None).unwrap();
        assert_eq!(sent, json!({ "id": TASK }), "{method}");
    }
}

#[test]
fn a_task_method_without_a_task_id_is_an_error() {
    for method in ["GetTask", "CancelTask", "SubscribeToTask"] {
        let error = params(method, None, None, None).unwrap_err().to_string();
        assert_eq!(
            error,
            format!("--method {method} requires --task-id"),
            "a method that needs a task id names the flag it needs"
        );
    }
}

#[test]
fn send_message_carries_text_task_id_and_label() {
    let sent = params("SendMessage", Some("hi"), Some(TASK), Some("name")).unwrap();
    let message = &sent["message"];
    assert_eq!(
        message["parts"][0]["text"], "hi",
        "the text becomes the message part"
    );
    assert_eq!(
        message["taskId"], TASK,
        "the task id is carried on the message"
    );
    assert_eq!(
        message["contextId"],
        MeshId::from(MESH).as_str(),
        "the context id is the mesh id"
    );
    assert_eq!(
        sent.as_object().unwrap().len(),
        1,
        "only the message is sent"
    );
}

#[test]
fn a_method_without_sugar_sends_an_empty_object() {
    let sent = params("ListTasks", Some("ignored"), Some(TASK), None).unwrap();
    assert_eq!(sent, json!({}), "an empty request sends an empty object");
}
