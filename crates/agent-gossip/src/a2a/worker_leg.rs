use serde::{Deserialize, Serialize};

use super::TaskState;
use super::status_update_params::Progress;
use super::worker_leg_error::WorkerLegError;

/// A worker's status leg, parsed once and owned from there to the wire.
///
/// A `working` leg is a check-in: it says what changed and how far the work is, and may say
/// that the worker waits on its own user. No other state takes a progress or that flag. Only
/// [`WorkerLeg::parse`] builds one, and a leg read from the IPC line goes through it too, so
/// the rule has one place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WorkerLegFields", into = "WorkerLegFields")]
#[non_exhaustive]
pub enum WorkerLeg {
    /// A check-in: the task is working.
    Checkin {
        /// What changed since the last check-in.
        note: String,
        /// How far the work is.
        progress: Progress,
        /// Whether the worker waits on its own user.
        waiting_user: bool,
    },
    /// Any other state: a question, a result, an end.
    Other {
        /// The state the leg moves the task to.
        state: TaskState,
        /// The status message, if any.
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
    /// A `working` leg with no text or no progress, and a progress or a waiting flag on any
    /// other state.
    pub fn parse(
        state: TaskState,
        note: Option<String>,
        progress: Option<Progress>,
        // `true` when the worker waits on its own user.
        waiting_user: bool,
    ) -> Result<Self, WorkerLegError> {
        if state == TaskState::Working {
            let trimmed: Option<String> = note.map(|raw| raw.trim().to_owned());
            let text = trimmed.filter(|text| !text.is_empty());
            let (Some(text), Some(progress)) = (text, progress) else {
                return Err(WorkerLegError::CheckinIncomplete);
            };
            return Ok(Self::Checkin {
                note: text,
                progress,
                waiting_user,
            });
        }
        if progress.is_some() {
            return Err(WorkerLegError::ProgressNotWorking);
        }
        if waiting_user {
            return Err(WorkerLegError::WaitingNotWorking);
        }
        Ok(Self::Other { state, note })
    }

    /// The state the leg moves the task to.
    #[must_use]
    pub fn state(&self) -> TaskState {
        match self {
            Self::Checkin { .. } => TaskState::Working,
            Self::Other { state, .. } => *state,
        }
    }

    /// The status message.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        match self {
            Self::Checkin { note, .. } => Some(note),
            Self::Other { note, .. } => note.as_deref(),
        }
    }

    /// How far the work is, on a check-in.
    #[must_use]
    pub fn progress(&self) -> Option<Progress> {
        match self {
            Self::Checkin { progress, .. } => Some(*progress),
            Self::Other { .. } => None,
        }
    }

    /// Whether the worker waits on its own user.
    #[must_use]
    pub fn waiting_user(&self) -> bool {
        matches!(
            self,
            Self::Checkin {
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
        match leg {
            WorkerLeg::Checkin {
                note,
                progress,
                waiting_user,
            } => Self {
                state: TaskState::Working,
                note: Some(note),
                progress: Some(progress),
                waiting_user,
            },
            WorkerLeg::Other { state, note } => Self {
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
