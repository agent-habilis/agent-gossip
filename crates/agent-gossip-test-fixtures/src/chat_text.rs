use agent_gossip::Message;

/// The text projection of a chat frame's A2A payload — what a test asserts
/// against, since the frame `body` carries the serialized payload.
pub fn chat_text(msg: &Message) -> String {
    agent_gossip::a2a::gossip::chat_text(msg).expect("a chat frame carries an a2a payload")
}
