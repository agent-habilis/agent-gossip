use crate::a2a::WorkerLegError;

/// The refusal of a status leg, worded with the names of the MCP tool's
/// arguments (`note`, `waiting_on_user`), not the CLI flags the error itself names.
pub(crate) fn worker_leg_message(error: WorkerLegError) -> &'static str {
    match error {
        WorkerLegError::CheckinIncomplete => "a working status needs a note and a progress",
        WorkerLegError::ProgressNotWorking => "progress needs the state working",
        WorkerLegError::WaitingNotWorking => "waiting_on_user needs the state working",
    }
}
