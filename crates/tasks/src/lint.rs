use xshell::{Shell, cmd};

use crate::TaskOutcome;

pub(crate) fn run(sh: &Shell) -> TaskOutcome {
    // `--features adversarial` so the adversarial suite + shim are linted too
    // (they are `required-features`-gated, else clippy skips them) — and so this
    // task matches `ci`'s clippy invocation. Without it, code reachable only
    // under the feature (e.g. `Message::new_state`, the extra `handle_session_request`
    // arms and their gated `expect`) trips false dead-code / too-many-lines errors.
    // The feature stays qualified (`agent-gossip/adversarial`) because a
    // workspace-wide invocation makes a bare feature name ambiguous.
    //
    // `--workspace`: clippy lints only the packages it is given. Naming the app
    // left the engine, the transport and the template crates compiled as
    // dependencies but never linted.
    cmd!(
        sh,
        "cargo clippy --workspace --all-targets --features agent-gossip/adversarial -- -D warnings"
    )
    .quiet()
    .run()?;
    Ok(())
}
