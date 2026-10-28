use tokio::sync::mpsc;

use super::Output;
use crate::events::OutputEvent;

/// A capturing sink paired with the receiver that drains it — the stream behind
/// [`MeshSession::events`](crate::api::MeshSession::events). Named apart from
/// [`Output::capture`], which builds only the sink half.
pub(crate) fn capture_events() -> (Output, mpsc::UnboundedReceiver<OutputEvent>) {
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    (Output::capture(events_tx), events_rx)
}
