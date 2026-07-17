use xshell::{Shell, cmd};

use crate::TaskOutcome;
use crate::util::{output, repo_root};

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    output::status("Installing", "agent-gossip");
    // Absolute, not `--path .`: no task changes directory, so a relative path
    // would only resolve when invoked from the workspace root.
    let pkg = repo_root().join("crates/agent-gossip");
    // `--force` is required: the crate version rarely changes between builds
    // (it stays `0.2.0` across many commits), and without `--force`
    // `cargo install` treats "0.2.0 already installed" as up-to-date
    // and **skips the rebuild entirely**, silently leaving the previously
    // installed binary in place. That shipped a stale `agent-gossip` to fleet hosts
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
    // Report what the installed binary *says it is*, not just that the install
    // ran: a fossilized vergen stamp shipped days of installs that all
    // self-identified as an old commit (`765793f`), and the mismatch was only
    // caught in a failing field test. Best-effort — a probe hiccup must not
    // fail an install that succeeded.
    let installed = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::home_dir().map(|user_home| user_home.join(".cargo")))
        .map_or_else(
            || "agent-gossip".into(),
            |cargo| cargo.join("bin/agent-gossip"),
        );
    let version = cmd!(sh, "{installed} --version")
        .quiet()
        .read()
        .unwrap_or_else(|_| "unknown (version probe failed)".to_owned());
    output::status("Installed", version.trim());
    Ok(())
}
