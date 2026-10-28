use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crate::COUNTER;

pub fn tmp_log(tag: &str) -> PathBuf {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "agent-gossip-test-{}-{}-{}.log",
        tag,
        std::process::id(),
        sequence
    ))
}
