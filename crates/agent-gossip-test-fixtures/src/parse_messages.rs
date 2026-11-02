use crate::ChatMessage;

/// `ChatMessage::to` is what separates them, so a test that cares asserts on it rather
/// than on which of the two arrived.
pub(crate) fn parse_messages(output: &str) -> Vec<ChatMessage> {
    let mut msgs = Vec::new();
    for line in output.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value["event"] != "message" || (value["type"] != "broadcast" && value["type"] != "msg") {
            continue;
        }
        let (Some(author), Some(body)) = (value["author"].as_str(), value["body"].as_str()) else {
            continue;
        };
        if !body.is_empty() {
            msgs.push(ChatMessage {
                author: author.to_string(),
                body: body.to_string(),
                to: value["to"].as_str().map(str::to_owned),
            });
        }
    }
    msgs
}
