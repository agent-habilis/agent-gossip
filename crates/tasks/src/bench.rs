use xshell::{Shell, cmd};

use crate::TaskOutcome;

/// Run the benchmark suite. No `.quiet()` — the point is to show the
/// divan table / transfer report.
///
/// - `cargo task bench` — everything (the divan microbenchmarks **and**
///   the ~100 MB transfer soak).
/// - `cargo task bench transfer` — only the loopback transfer soak.
/// - `cargo task bench <filter>` — only the microbenchmarks, with
///   `<filter>` passed to divan (e.g. `cargo task bench derive_secret`).
///
/// The `bench` feature exposes the in-crate `harness::bench` shim the
/// microbenchmarks need; the transfer bench uses only the public API.
pub(crate) fn run(sh: &Shell, args: &[String]) -> TaskOutcome {
    match args.first().map(String::as_str) {
        None => cmd!(sh, "cargo bench -p agent-square --features bench").run()?,
        Some("transfer") => cmd!(sh, "cargo bench -p agent-square --bench transfer").run()?,
        _ => cmd!(
            sh,
            "cargo bench -p agent-square --features bench --bench hot_paths -- {args...}"
        )
        .run()?,
    }
    Ok(())
}
