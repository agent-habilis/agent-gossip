/// Who is following up, into which task, on whom — grouped so
/// [`cli_task_followup`] stays inside the argument budget.
#[derive(Debug)]
#[non_exhaustive]
pub struct FollowupParams<'caller> {
    pub mesh: &'caller str,
    pub nickname: &'caller str,
    pub to: &'caller str,
    pub task_id: &'caller str,
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

    #[must_use]
    pub fn with_text(mut self, text: &'caller str) -> Self {
        self.text = text;
        self
    }
}
