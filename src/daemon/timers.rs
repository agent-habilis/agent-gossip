//! Time-driven housekeeping ticks with no subsystem of their own:
//! rate-limiter pruning and the state-file liveness heartbeat. The
//! `Alive`/sweep heartbeat ticks live in `lifecycle::heartbeat`; the
//! gossip healer in `gossip::heal`.

use std::time::{Duration, Instant};

use super::state::EventLoopState;

/// Rate-limiter pruning: runs every `PRUNE_INTERVAL_SECS` (see `run`).
pub(crate) fn tick_prune(state: &mut EventLoopState) {
    state.rate_limiter.prune_inactive();
}

/// Re-asserts `participant_count` + `last_updated` into the state
/// file so external readers can treat a fresh `last_updated` as
/// liveness. No-op without `--state-file`.
///
/// Also logs a neighbor census: `meshed` yet zero peer links while the
/// roster is non-empty is the silent-partition signature, surfaced at
/// `warn` for post-freeze diagnosis.
pub(crate) fn tick_state_refresh(state: &EventLoopState) {
    state.write_participant_count();
    let roster_len = state.participants.len();
    let link_len = state.linked_endpoints.len();
    if state.meshed && link_len == 0 && roster_len > 0 {
        tracing::warn!(
            target: "agent_habilis_swarm::lifecycle",
            roster_len,
            link_len,
            meshed = state.meshed,
            "neighbor census: silent partition (meshed but zero peer links)"
        );
    } else {
        tracing::debug!(
            target: "agent_habilis_swarm::lifecycle",
            roster_len,
            link_len,
            meshed = state.meshed,
            "neighbor census"
        );
    }
}

/// Log a maintenance timer's interval fidelity and advance `*last`. A
/// gap far over `expected` is the throttle/freeze signature (the prime
/// suspect for the silent mesh collapse), so it is `warn`. Callers
/// needing the gap (the heal arm) read it before calling this.
pub(crate) fn note_tick_gap(timer: &'static str, last: &mut Instant, expected: Duration) {
    let now = Instant::now();
    let gap = now.duration_since(*last);
    *last = now;
    let gap_ms = u64::try_from(gap.as_millis()).unwrap_or(u64::MAX);
    let expected_ms = u64::try_from(expected.as_millis()).unwrap_or(u64::MAX);
    if gap > expected.saturating_mul(2) {
        tracing::warn!(
            target: "agent_habilis_swarm::lifecycle",
            timer,
            gap_ms,
            expected_ms,
            "maintenance timer stalled (throttle/freeze suspected)"
        );
    } else {
        tracing::debug!(
            target: "agent_habilis_swarm::lifecycle",
            timer,
            gap_ms,
            expected_ms,
            "maintenance tick"
        );
    }
}
