/// The A2A spec revision these objects conform to.
///
/// Carried in every [`AgentCard`](crate::a2a::model::AgentCard)'s interfaces so clients can gate on it.
/// v1.0 encodes every
/// object as `ProtoJSON` (`SCREAMING_SNAKE` enums, no inline `kind` tags).
pub const PROTOCOL_VERSION: &str = "1.0";
