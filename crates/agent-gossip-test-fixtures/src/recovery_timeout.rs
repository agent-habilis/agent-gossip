use std::time::Duration;

/// Budget for a delivery asserted **after a disruption**.
///
/// A disruption is a beacon death,
/// SIGSTOP freeze, rendezvous migration, or creator departure. Re-meshing waits
/// on the heal cadence (production 15s; the reliability tests inject a short
/// `--heal-interval-secs`), so a recovery that just missed a tick needs
/// another cycle — the steady-state `MSG_TIMEOUT` (1 min) sits right on that
/// cliff and flakes on a loaded host. 2 min clears many cycles; `wait_until`
/// is adaptive, so a healthy run returns in seconds and only a genuine stall
/// pays the ceiling. The extra headroom over a bare "few cycles" is for a host
/// running real mesh daemons alongside the suite (dogfooding) — CPU starvation
/// there slows convergence past a tighter bound without any product fault. One
/// named constant so every post-disruption assertion across the suite uses the
/// same floor.
pub const RECOVERY_TIMEOUT: Duration = Duration::from_mins(2);
