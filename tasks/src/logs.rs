use crate::TaskOutcome;

pub(crate) fn run() -> TaskOutcome {
    // Mirrors `LOG_SUBPATH` in the main crate's `util::consts` (the daemon's
    // default log dir). Duplicated as a bare literal — deliberately — so this
    // dev-only task runner never links the daemon (iroh/tokio) just to read
    // one path. The default never changes; the `--log-dir` override is a
    // daemon-side test knob and irrelevant to printing the default here.
    let dir = std::env::temp_dir().join("agent-habilis/swarm/logs");
    // Ensure it exists so `cd`/`tail` never fail on a fresh machine.
    std::fs::create_dir_all(&dir)?;
    // stdout, the sole output, so `$(cargo task logs)` captures just the path.
    println!("{}", dir.display());
    Ok(())
}
