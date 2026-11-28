use serde::{Deserialize, Serialize};

use super::TaskState;
use super::status_update_params::Progress;
use super::worker_leg_error::WorkerLegError;

/// A worker's status leg, parsed once and owned from there to the wire.
///
/// A `working` leg is a check-in: it says what changed and how far the work is, and may say
/// that the worker waits on its own user. No other state takes a progress or that flag. Only
/// [`WorkerLeg::parse`] and [`WorkerLeg::canceled`] build one, and a leg read from the IPC line
/// goes through the parse too, so the rule has one place and no other code can hold a bare
/// `working` leg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WorkerLegFields", into = "WorkerLegFields")]
pub struct WorkerLeg(Leg);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Leg {
    /// A check-in: the task is working.
    Checkin {
        note: String,
        progress: Progress,
        waiting_user: bool,
    },
    /// Any other state: a question, a result, an end.
    Other {
        state: TaskState,
        note: Option<String>,
    },
}

/// The flat shape of a [`WorkerLeg`] on the IPC line.
#[derive(Serialize, Deserialize)]
struct WorkerLegFields {
    #[serde(with = "super::friendly_state")]
    state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress: Option<Progress>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    waiting_user: bool,
}

impl WorkerLeg {
    /// Parse the fields of a status request.
    ///
    /// # Errors
    /// A `working` leg with no text or no progress, a progress or a waiting flag on any other
    /// state, and a state that a worker never sends (`submitted`, `auth-required`, `unspecified`).
    pub fn parse(
        state: TaskState,
        note: Option<String>,
        progress: Option<Progress>,
        // `true` when the worker waits on its own user.
        waiting_user: bool,
    ) -> Result<Self, WorkerLegError> {
        if matches!(
            state,
            TaskState::Submitted | TaskState::AuthRequired | TaskState::Unspecified
        ) {
            return Err(WorkerLegError::NotAWorkerState);
        }
        if state == TaskState::Working {
            let trimmed: Option<String> = note.map(|raw| raw.trim().to_owned());
            let text = trimmed.filter(|text| !text.is_empty());
            let (Some(text), Some(progress)) = (text, progress) else {
                return Err(WorkerLegError::CheckinIncomplete);
            };
            return Ok(Self(Leg::Checkin {
                note: text,
                progress,
                waiting_user,
            }));
        }
        if progress.is_some() {
            return Err(WorkerLegError::ProgressNotWorking);
        }
        if waiting_user {
            return Err(WorkerLegError::WaitingNotWorking);
        }
        Ok(Self(Leg::Other { state, note }))
    }

    /// The leg that ends a task the worker will not finish, with `note` as the reason.
    #[must_use]
    pub fn canceled(note: &str) -> Self {
        Self(Leg::Other {
            state: TaskState::Canceled,
            note: Some(note.to_owned()),
        })
    }

    /// The state the leg moves the task to.
    #[must_use]
    pub fn state(&self) -> TaskState {
        match &self.0 {
            Leg::Checkin { .. } => TaskState::Working,
            Leg::Other { state, .. } => *state,
        }
    }

    /// The status message.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        match &self.0 {
            Leg::Checkin { note, .. } => Some(note),
            Leg::Other { note, .. } => note.as_deref(),
        }
    }

    /// How far the work is, on a check-in.
    #[must_use]
    pub fn progress(&self) -> Option<Progress> {
        match &self.0 {
            Leg::Checkin { progress, .. } => Some(*progress),
            Leg::Other { .. } => None,
        }
    }

    /// Whether the worker waits on its own user.
    #[must_use]
    pub fn waiting_user(&self) -> bool {
        matches!(
            self.0,
            Leg::Checkin {
                waiting_user: true,
                ..
            }
        )
    }
}

impl TryFrom<WorkerLegFields> for WorkerLeg {
    type Error = String;

    fn try_from(fields: WorkerLegFields) -> Result<Self, Self::Error> {
        let WorkerLegFields {
            state,
            note,
            progress,
            waiting_user,
        } = fields;
        Self::parse(state, note, progress, waiting_user).map_err(|error| error.to_string())
    }
}

impl From<WorkerLeg> for WorkerLegFields {
    fn from(leg: WorkerLeg) -> Self {
        match leg.0 {
            Leg::Checkin {
                note,
                progress,
                waiting_user,
            } => Self {
                state: TaskState::Working,
                note: Some(note),
                progress: Some(progress),
                waiting_user,
            },
            Leg::Other { state, note } => Self {
                state,
                note,
                progress: None,
                waiting_user: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TaskState, WorkerLeg};
    use crate::a2a::status_update_params::Progress;

    fn progress(percent: &str) -> Progress {
        percent.parse().expect("the test passes a valid percentage")
    }

    /// The parse refuses a flag or a progress on any state but `working`, so a library caller
    /// cannot leave `waiting_user` on an `input-required` task.
    #[test]
    fn the_flag_and_a_progress_need_the_working_state() {
        assert!(
            WorkerLeg::parse(
                TaskState::Working,
                Some("built it".to_owned()),
                Some(progress("40")),
                true
            )
            .is_ok(),
            "a check-in may wait on its user while working"
        );
        assert!(
            WorkerLeg::parse(TaskState::InputRequired, None, None, false).is_ok(),
            "an input-required leg parses without the flag"
        );
        for state in [
            TaskState::InputRequired,
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Canceled,
            TaskState::Rejected,
        ] {
            let flag = WorkerLeg::parse(state, None, None, true)
                .expect_err("the flag with a non-working state must be refused");
            assert_eq!(
                flag.to_string(),
                "waiting-on-user requires state working",
                "the flag needs the working state"
            );
            let percent = WorkerLeg::parse(state, None, Some(progress("5")), false)
                .expect_err("a progress with a non-working state must be refused");
            assert_eq!(
                percent.to_string(),
                "progress requires state working",
                "a progress needs the working state"
            );
        }
    }

    /// A worker never sends `submitted` (the opening), `auth-required` or `unspecified`.
    #[test]
    fn a_state_that_a_worker_never_sends_is_refused() {
        for state in [
            TaskState::Submitted,
            TaskState::AuthRequired,
            TaskState::Unspecified,
        ] {
            let error = WorkerLeg::parse(state, Some("note".to_owned()), None, false)
                .expect_err("a worker has no such state to report");
            assert_eq!(
                error.to_string(),
                "state must be working, input-required, completed, failed, canceled or rejected",
                "{state:?} is named as a state no worker sends"
            );
        }
    }

    /// A bare `working` is refused: a check-in says what changed and how far the work is.
    #[test]
    fn a_bare_working_leg_is_refused() {
        for (note, percent) in [
            (None, None),
            (Some("built it"), None),
            (None, Some(progress("10"))),
            (Some("   "), Some(progress("10"))),
        ] {
            let error =
                WorkerLeg::parse(TaskState::Working, note.map(str::to_owned), percent, false)
                    .expect_err("a working leg without text and progress must be refused");
            assert_eq!(
                error.to_string(),
                "working requires a text and a progress",
                "the rule names both fields"
            );
        }
    }

    /// A leg read off the IPC line goes through the same rule, so the daemon never holds one
    /// the sender would have refused.
    #[test]
    fn a_leg_on_the_ipc_line_obeys_the_same_rule() {
        let leg = WorkerLeg::parse(
            TaskState::Working,
            Some("built it".to_owned()),
            Some(progress("40")),
            true,
        )
        .expect("a full check-in parses");
        let line = serde_json::to_string(&leg).expect("a leg serializes to JSON");
        assert_eq!(
            serde_json::from_str::<WorkerLeg>(&line).ok(),
            Some(leg),
            "a leg survives the line"
        );
        for bare in [
            r#"{"state":"working"}"#,
            r#"{"state":"input-required","waiting_user":true}"#,
            r#"{"state":"completed","progress":50}"#,
        ] {
            assert!(
                serde_json::from_str::<WorkerLeg>(bare).is_err(),
                "{bare} breaks the rule and must not parse"
            );
        }
    }
}
