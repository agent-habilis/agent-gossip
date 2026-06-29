use crate::TaskOutcome;

pub(crate) fn run() -> TaskOutcome {
    // Mirrors `RUNTIME_DIR` in the main crate's `util::consts` (the base for
    // per-swarm log/socket/state folders). Duplicated as a bare literal —
    // deliberately — so this dev-only task runner never links the daemon
    // (iroh/tokio) just to read one path. The default never changes; the
    // `--log-dir` override is a daemon-side test knob, irrelevant here.
    let dir = std::path::PathBuf::from("/tmp/agent-habilis/swarm");
    // Ensure it exists so `cd`/`tail` never fail on a fresh machine.
    std::fs::create_dir_all(&dir)?;
    // stdout, the sole output, so `$(cargo task logs)` captures just the path.
    println!("{}", dir.display());
    Ok(())
}
