use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::{output, repo_root};

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Installing", "agent-square");
    // Absolute, not `--path .`: no task changes directory, so a relative path
    // would only resolve when invoked from the workspace root.
    let pkg = repo_root().join("crates/agent-square");
    // `--force` is required: the crate version rarely changes between builds
    // (it stays `0.2.0` across many commits), and without `--force`
    // `cargo install` treats "0.2.0 already installed" as up-to-date
    // and **skips the rebuild entirely**, silently leaving the previously
    // installed binary in place. That shipped a stale `agent-square` to fleet hosts
    // (the binary's git-hash stamp lagged the checked-out commit). `--force`
    // always rebuilds + reinstalls the current tree.
    //
    // `--locked` is equally load-bearing: without it `cargo install`
    // ignores Cargo.lock and freshly resolves on the host, so a registry
    // release after the lock was cut can change the build — a 2026-06-12
    // fleet install failed outright on a `time` upgrade whose trait impls
    // collided with iroh-gossip's blanket `From`.
    cmd!(sh, "cargo install --path {pkg} --force --locked")
        .quiet()
        .run()?;
    output::status("Installed", "~/.cargo/bin/agent-square");
    Ok(())
}
