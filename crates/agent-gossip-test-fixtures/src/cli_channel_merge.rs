use std::process::Output;

use agent_gossip::Channel;

use crate::{channel_subcommand, test_cmd};

/// Spawn `agent-gossip <channel> merge …`, returning the raw
/// [`Output`](std::process::Output). The CLI **exits non-zero** on a rejected
/// `{ok:false}` merge (the scriptable exit-code contract), so this returns the
/// status + stdout unjudged for the caller to assert on.
///
/// # Panics
///
/// Panics if the CLI process fails to spawn.
#[must_use]
pub fn cli_channel_merge(channel: Channel, mesh: &str, nickname: &str, merge: &str) -> Output {
    test_cmd()
        .args([channel_subcommand(channel), "merge"])
        .args(["--gossip", mesh, "--nickname", nickname, "--merge", merge])
        .output()
        .expect("channel merge failed to spawn")
}
