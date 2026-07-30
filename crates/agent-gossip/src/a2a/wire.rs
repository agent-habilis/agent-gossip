// The two chat tags differ only in addressing, and the addressing is what makes
// them separate tags rather than one tag with an optional `to`: the classifier
// can then reject a directed BROADCAST (a private line dressed as gossip-wide)
// and a broadcast MSG (a msg leaked to everyone) on the tag alone.
pub(crate) const BROADCAST: &str = "a2a_broadcast";
pub(crate) const MSG: &str = "a2a_msg";
pub(crate) const STATUS: &str = "a2a_status";
pub(crate) const ARTIFACT: &str = "a2a_artifact";
pub(crate) const REQ: &str = "a2a_req";
pub(crate) const RESP: &str = "a2a_resp";
