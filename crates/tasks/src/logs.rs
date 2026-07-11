use crate::TaskOutcome;

pub(crate) fn run() -> TaskOutcome {
    // Create + validate the per-user runtime base via the *same* helper the
    // daemon uses (this task runner already links the crate for `cargo task
    // man`), so `cargo task logs` never diverges from where the daemon writes,
    // never weakens the 0700 mode, and applies the same symlink/ownership guard.
    let dir = agent_square::ensure_runtime_base()?;
    // stdout, the sole output, so `$(cargo task logs)` captures just the path.
    println!("{}", dir.display());
    Ok(())
}
