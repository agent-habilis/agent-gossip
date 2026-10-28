use std::fs::{self};

use crate::test_log_dir;

/// A node's tracing-sink log (distinct from its captured stdout/stderr
/// in `Node::log`). Mirrors `agent_gossip::logs::log_file_path`:
/// `<mesh_prefix>/<nick>.tracing.log` under the per-test log dir. Use this to
/// assert on `tracing` output (warn/info) the operator stream never carries.
pub fn trace_log(mesh: &str, nickname: &str) -> String {
    let path = format!(
        "{}/{}/{nickname}.tracing.log",
        test_log_dir(),
        agent_gossip::mesh_prefix(mesh)
    );
    fs::read_to_string(path).unwrap_or_default()
}
