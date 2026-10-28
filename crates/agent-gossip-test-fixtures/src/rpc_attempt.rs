use std::time::Duration;

/// Per-attempt deadline for a directed A2A call (see [`InProcNode::a2a_call`]).
/// Deliberately far below `MSG_TIMEOUT`: a request the overlay dropped will
/// never be answered, so the only thing a long wait buys is a slower retry. Long
/// enough that a merely *slow* peer still answers within one attempt.
pub(crate) const RPC_ATTEMPT: Duration = Duration::from_secs(10);
