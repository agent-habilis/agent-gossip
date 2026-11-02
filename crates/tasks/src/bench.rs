use crate::TaskOutcome;
use crate::util::Invocation;

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
pub(crate) fn run(args: &[String]) -> TaskOutcome {
    let invocation = match args.first().map(String::as_str) {
        None => Invocation::cargo(["bench", "-p", "agent-gossip", "--features", "bench"]),
        Some("transfer") => {
            Invocation::cargo(["bench", "-p", "agent-gossip", "--bench", "transfer"])
        }
        _ => Invocation::cargo([
            "bench",
            "-p",
            "agent-gossip",
            "--features",
            "bench",
            "--bench",
            "hot_paths",
            "--",
        ])
        .args(args),
    };
    invocation.run()
}
