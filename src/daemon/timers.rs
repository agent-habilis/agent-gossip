//! Time-driven housekeeping ticks with no subsystem of their own:
//! rate-limiter pruning and the state-file liveness heartbeat. The
//! `Alive`/sweep heartbeat ticks live in `lifecycle::heartbeat`; the
//! gossip healer in `gossip::heal`.

use super::state::EventLoopState;

/// Rate-limiter pruning: runs every `PRUNE_INTERVAL_SECS` (see `run`).
pub(crate) fn tick_prune(state: &mut EventLoopState) {
    state.rate_limiter.prune_inactive();
}

/// Heartbeat that re-asserts `participant_count` + `last_updated`
/// into the session state file on a fixed cadence even when
/// membership is unchanged, so external readers can treat a fresh
/// `last_updated` as liveness. No-op when no `--state-file` is set.
pub(crate) fn tick_state_refresh(state: &EventLoopState) {
    state.write_participant_count();
}
