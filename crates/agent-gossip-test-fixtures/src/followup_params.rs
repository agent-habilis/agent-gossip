/// Who is following up, into which task, on whom — grouped so
/// [`cli_task_followup`] stays inside the argument budget.
#[derive(Debug)]
pub struct FollowupParams<'a> {
    pub mesh: &'a str,
    pub nickname: &'a str,
    pub to: &'a str,
    pub task_id: &'a str,
    pub text: &'a str,
}
