use serde::Serialize;

use crate::api::SessionError;

#[derive(Debug, Serialize)]
pub(crate) struct LeaveResult {
    pub(crate) ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

/// What the `leave_gossip` tool answers for the outcome of the leave.
impl From<Result<(), SessionError>> for LeaveResult {
    fn from(outcome: Result<(), SessionError>) -> Self {
        match outcome {
            Ok(()) => Self {
                ok: true,
                error: None,
            },
            Err(error) => Self {
                ok: false,
                error: Some(error.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::LeaveResult;
    use crate::api::SessionError;

    #[test]
    fn a_failed_leave_answers_ok_false_with_the_text_of_the_error() {
        let text = "the node was not closed in 14.5 s";
        let outcome = Err(SessionError::LeaveFailed(anyhow!(text)));
        let result = LeaveResult::from(outcome);
        assert!(
            !result.ok,
            "a leave that did not close the node must not answer ok"
        );
        assert_eq!(
            result.error.as_deref(),
            Some(text),
            "the tool tells the agent why the leave failed"
        );
    }

    #[test]
    fn a_leave_that_ended_answers_ok_true_with_no_error() {
        let result = LeaveResult::from(Ok(()));
        assert!(result.ok, "a closed node answers ok");
        assert_eq!(result.error, None, "a closed node has no error text");
    }
}
