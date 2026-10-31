use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct PingResult {
    /// Peers that answered, each `{nickname, rtt_ms}` (round-trip milliseconds).
    pub(crate) peers: Vec<crate::emission::PingPeer>,
}
