/// An RPC `message/send` task leg with no backing frame — the fields
/// [`Output::task_message`] and [`simple_event::format_task_message_json`] need,
/// bundled (they mirror the [`OutputEvent::TaskMessage`] variant's fields).
pub(crate) struct TaskMessageLeg<'a> {
    pub id: &'a str,
    pub mesh: &'a str,
    pub author: &'a str,
    pub peer: &'a str,
    pub task_id: &'a str,
    pub state: Option<crate::a2a::TaskState>,
    pub text: &'a str,
    pub label: Option<&'a str>,
    pub message: Option<&'a crate::a2a::Message>,
    pub is_self: bool,
}
