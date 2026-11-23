use std::time::Duration;

/// Per-attempt deadline for a directed A2A call (see [`InProcNode::a2a_call`](crate::InProcNode::a2a_call)).
/// Deliberately far below `MSG_TIMEOUT`: a request the overlay dropped will
/// never be answered, so the only thing a long wait buys is a slower retry. Long
/// enough that a merely *slow* peer still answers within one attempt.
///
/// This is shorter than the 18 s that a send to a peer with a dead path can wait for the verdict of the
/// engine, on purpose: an attempt is a budget for 1 try and not for the send. The loops that use it retry
/// until `MSG_TIMEOUT` (60 s), which is above the 18 s, so no loop ends before one send has its verdict.
pub(crate) const RPC_ATTEMPT: Duration = Duration::from_secs(10);
