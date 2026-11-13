use std::time::Instant;

use agent_gossip::Channel;

use crate::{MSG_TIMEOUT, POLL, cli_channel_get, cli_channel_merge, meta_slot_of};

/// Write the entry of `nickname` into its own slice of the `meta` channel, as the join skill does.
/// The daemon writes no entry by itself.
///
/// # Panics
///
/// Panics if the merge is refused.
pub fn cli_meta_report(mesh: &str, nickname: &str) {
    let merge = serde_json::json!({ "nickname": nickname, "status": "idle" }).to_string();
    let output = cli_channel_merge(Channel::Meta, mesh, nickname, &merge);
    assert!(
        output.status.success(),
        "the daemon of {nickname} refused its own meta report: {output:?}"
    );
}

/// Block until `observer` holds the entry that `target` reported, and return its slot: the hex of
/// the author key of `target`.
///
/// # Panics
///
/// Panics if no entry of `target` reaches `observer` before `MSG_TIMEOUT`.
#[must_use]
pub fn await_meta_slot_cli(mesh: &str, observer: &str, target: &str) -> String {
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let raw = cli_channel_get(Channel::Meta, mesh, observer);
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
            && let Some(slot) = meta_slot_of(&value["document"], target)
        {
            return slot;
        }
        assert!(
            Instant::now() < deadline,
            "no meta entry of {target} reached {observer}"
        );
        std::thread::sleep(POLL);
    }
}
