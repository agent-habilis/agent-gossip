/// An RPC `message/send` task leg with no backing frame — the fields
/// `Output::task_message` and `simple_event::format_task_message_json` need,
/// bundled (they mirror the `OutputEvent::TaskMessage` variant's fields).
pub(crate) struct TaskMessageLeg<'source> {
    pub id: &'source str,
    pub mesh: &'source str,
    pub author: &'source str,
    pub peer: &'source str,
    pub task_id: &'source str,
    pub state: Option<crate::a2a::TaskState>,
    pub text: &'source str,
    pub label: Option<&'source str>,
    pub message: Option<&'source crate::a2a::Message>,
    pub is_self: bool,
}
