/// Who is following up, into which task, on whom — grouped so
/// [`cli_task_followup`](fn@crate::cli_task_followup) stays inside the argument budget.
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
    /// A file that the follow-up carries, offloaded over the blob channel.
    pub file: Option<&'caller std::path::Path>,
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
            file: None,
        }
    }

    /// Set the body of the follow-up; the constructor leaves it empty.
    #[must_use]
    pub fn with_text(mut self, text: &'caller str) -> Self {
        self.text = text;
        self
    }

    /// Attach `file` to the follow-up; the constructor attaches none.
    #[must_use]
    pub fn with_file(mut self, file: &'caller std::path::Path) -> Self {
        self.file = Some(file);
        self
    }
}
