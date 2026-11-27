use std::fmt;

/// Why a status request is not a [`WorkerLeg`](super::WorkerLeg).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkerLegError {
    /// A `working` leg is a check-in, and it needs a text and a progress.
    CheckinIncomplete,
    /// A progress goes only with the `working` state.
    ProgressNotWorking,
    /// The flag that the worker waits on its user goes only with the `working` state.
    WaitingNotWorking,
}

impl fmt::Display for WorkerLegError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CheckinIncomplete => "working requires a text and a progress",
            Self::ProgressNotWorking => "progress requires state working",
            Self::WaitingNotWorking => "waiting-on-user requires state working",
        })
    }
}

impl std::error::Error for WorkerLegError {}
