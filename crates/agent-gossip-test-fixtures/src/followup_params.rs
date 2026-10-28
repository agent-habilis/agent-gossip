/// Who is following up, into which task, on whom — grouped so
/// [`cli_task_followup`] stays inside the argument budget.
#[derive(Debug)]
#[non_exhaustive]
pub struct FollowupParams<'caller> {
    /// The mesh id.
    pub mesh: &'caller str,
    /// The nickname of the daemon that sends the follow-up.
    pub nickname: &'caller str,
    /// The nickname of the peer that receives it.
    pub to: &'caller str,
    /// The task the follow-up joins.
    pub task_id: &'caller str,
    /// The body of the follow-up.
    pub text: &'caller str,
}

impl<'caller> FollowupParams<'caller> {
    /// A follow-up with an empty text; add it with [`with_text`](Self::with_text).
    #[must_use]
    pub fn new(
        mesh: &'caller str,
        nickname: &'caller str,
        to: &'caller str,
        task_id: &'caller str,
    ) -> Self {
        Self {
            mesh,
            nickname,
            to,
            task_id,
            text: "",
        }
    }

    /// Set the body of the follow-up; the constructor leaves it empty.
    #[must_use]
    pub fn with_text(mut self, text: &'caller str) -> Self {
        self.text = text;
        self
    }
}
