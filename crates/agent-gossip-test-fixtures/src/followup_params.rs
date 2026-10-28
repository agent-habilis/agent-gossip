/// Who is following up, into which task, on whom — grouped so
/// [`cli_task_followup`] stays inside the argument budget.
#[derive(Debug)]
pub struct FollowupParams<'caller> {
    pub mesh: &'caller str,
    pub nickname: &'caller str,
    pub to: &'caller str,
    pub task_id: &'caller str,
    pub text: &'caller str,
}
