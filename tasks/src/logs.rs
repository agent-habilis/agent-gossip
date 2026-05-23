use crate::TaskOutcome;

pub(crate) fn run() -> TaskOutcome {
    let dir = ahs_shared::logs::log_dir();
    // Ensure it exists so `cd`/`tail` never fail on a fresh machine.
    std::fs::create_dir_all(&dir)?;
    // stdout, the sole output, so `$(cargo task logs)` captures just the path.
    println!("{}", dir.display());
    Ok(())
}
