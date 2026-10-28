use agent_gossip::Message;

/// The text projection of a chat frame's A2A payload — what a test asserts
/// against, since the frame `body` carries the serialized payload.
pub fn chat_text(message: &Message) -> String {
    agent_gossip::a2a::status_update_params::chat_text(message)
        .expect("a chat frame carries an a2a payload")
}
