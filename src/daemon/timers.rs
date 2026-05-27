//! Time-driven housekeeping ticks with no subsystem of their own:
//! rate-limiter pruning and the state-file liveness heartbeat. The
//! `Alive`/sweep heartbeat ticks live in `lifecycle::heartbeat`; the
//! gossip healer in `gossip::heal`.

use std::time::{Duration, Instant};

use iroh::Endpoint;

use super::state::EventLoopState;
use crate::output;
use crate::util::{rss, tuning};

/// Rate-limiter pruning + the warn-only RSS leak check: runs every
/// `PRUNE_INTERVAL_SECS` (see `run`).
pub(crate) fn tick_prune(state: &mut EventLoopState, output: &output::Output) {
    state.rate_limiter.prune_inactive();
    warn_on_high_rss(state, output, tuning::rss_warn_mb());
}

/// One-shot leak-visibility signal: if RSS has crossed `threshold_mb`
/// (`consts::RSS_WARN_MB`), emit a `warn` (file sink) plus a JSON `info` event
/// (`--output json` stream) exactly once. Warn-only — never exits or alters
/// behavior; the distributed soak that crashed a host had no such in-process
/// signal. `threshold_mb` is passed in (not read here) so the threshold stays
/// a single source of truth and the latch is unit-testable without env races.
fn warn_on_high_rss(state: &mut EventLoopState, output: &output::Output, threshold_mb: u64) {
    if threshold_mb == 0 || state.rss_warned {
        return;
    }
    let Some(rss_bytes) = rss::peak_rss_bytes() else {
        return;
    };
    let rss_mb = rss_bytes / (1024 * 1024);
    if rss_mb >= threshold_mb {
        state.rss_warned = true;
        tracing::warn!(
            target: "agent_habilis_swarm::lifecycle",
            rss_mb,
            threshold_mb,
            "RSS crossed the warn threshold (possible leak); not exiting — see consts::RSS_WARN_MB"
        );
        output.info(&format!(
            "RSS {rss_mb} MiB crossed warn threshold {threshold_mb} MiB (possible memory leak)"
        ));
    }
}

/// Re-asserts `participant_count` + `last_updated` into the state
/// file so external readers can treat a fresh `last_updated` as
/// liveness. No-op without `--state-file`.
///
/// Also logs a neighbor census: `meshed` yet zero peer links while the
/// roster is non-empty is the silent-partition signature, surfaced at
/// `warn` for post-freeze diagnosis. At `debug` it also classifies each
/// live peer link `direct`/`relay`/`mixed` (see
/// [`crate::gossip::conn_path`]) — this periodic reading is the
/// representative one (post hole-punch upgrade), unlike the `NeighborUp`
/// snapshot which skews `relay`. Gated on `debug` being live so the
/// per-peer `remote_info` round-trips are skipped at the default level.
pub(crate) async fn tick_state_refresh(state: &EventLoopState, endpoint: &Endpoint) {
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
        return;
    }

    // Always-on census reading: the cheap counts only (no `remote_info`
    // round-trips), at `info` so the file sink carries a per-tick
    // roster/link time series — the flap rate read directly, not
    // inferred from counting NeighborUp/Down lines. `peak_rss_mb` rides the
    // same line as the leak gauge: peak RSS is monotonic, so a
    // churn-proportional leak reads as a rising slope right next to the
    // flap counts — no external `ps` sampler needed (0 if RSS is
    // unreadable on this platform).
    tracing::info!(
        target: "agent_habilis_swarm::lifecycle",
        roster_len,
        link_len,
        meshed = state.meshed,
        peak_rss_mb = rss::peak_rss_mb().unwrap_or(0),
        "mesh census"
    );

    // The per-peer conn classification feeds only the DEBUG census
    // below, and each `conn_path` is a `remote_info` actor round-trip.
    // `lifecycle` is pinned to `info` by default, so skip the whole loop
    // unless DEBUG is actually live — otherwise the tick pays N
    // round-trips every interval for output the subscriber discards.
    if !tracing::enabled!(target: "agent_habilis_swarm::lifecycle", tracing::Level::DEBUG) {
        return;
    }
    let (mut direct, mut relay, mut other) = (0usize, 0usize, 0usize);
    for &peer_id in &state.linked_endpoints {
        let (conn, relay_url) = crate::gossip::conn_path(endpoint, peer_id).await;
        match conn {
            "direct" => direct += 1,
            "relay" => relay += 1,
            _ => other += 1,
        }
        tracing::debug!(
            target: "agent_habilis_swarm::lifecycle",
            endpoint_id = %peer_id,
            conn,
            relay = relay_url.as_ref().map_or("-", |url| url.as_str()),
            "peer conn path"
        );
    }
    tracing::debug!(
        target: "agent_habilis_swarm::lifecycle",
        roster_len,
        link_len,
        meshed = state.meshed,
        direct,
        relay,
        other,
        "neighbor census"
    );
}

/// Log a maintenance timer's interval fidelity and advance both its
/// monotonic and wall-clock anchors; returns `(mono_gap, wall_gap)` for
/// the heal arm, which drives the resume-edge re-bootstrap off them.
///
/// Two stall signatures are surfaced at `warn` (the prime suspects for
/// a silent mesh collapse): a monotonic gap far over `expected` (the
/// process was throttled), and a wall-vs-monotonic divergence far over
/// `expected` (the process was *suspended* — macOS sleep freezes the
/// monotonic clock in lockstep, so only the wall clock reveals it).
pub(crate) fn note_tick_gap(
    timer: &'static str,
    last: &mut Instant,
    last_wall: &mut i64,
    expected: Duration,
) -> (Duration, Duration) {
    let now = Instant::now();
    let mono_gap = now.duration_since(*last);
    *last = now;

    let now_wall = crate::util::clock::unix_secs();
    let wall_gap =
        Duration::from_secs(u64::try_from(now_wall.saturating_sub(*last_wall)).unwrap_or(0));
    *last_wall = now_wall;

    let mono_gap_ms = u64::try_from(mono_gap.as_millis()).unwrap_or(u64::MAX);
    let wall_gap_ms = u64::try_from(wall_gap.as_millis()).unwrap_or(u64::MAX);
    let expected_ms = u64::try_from(expected.as_millis()).unwrap_or(u64::MAX);
    let throttled = mono_gap > expected.saturating_mul(2);
    let suspended = wall_gap.saturating_sub(mono_gap) > expected.saturating_mul(2);
    if throttled || suspended {
        tracing::warn!(
            target: "agent_habilis_swarm::lifecycle",
            timer,
            mono_gap_ms,
            wall_gap_ms,
            expected_ms,
            suspected = if suspended { "freeze/sleep" } else { "throttle" },
            "maintenance timer stalled"
        );
    } else {
        tracing::debug!(
            target: "agent_habilis_swarm::lifecycle",
            timer,
            mono_gap_ms,
            wall_gap_ms,
            expected_ms,
            "maintenance tick"
        );
    }
    (mono_gap, wall_gap)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use tokio::sync::mpsc::unbounded_channel;

    use super::{EventLoopState, output, warn_on_high_rss};
    use crate::protocol::identity::Identity;

    fn fresh_state() -> EventLoopState {
        EventLoopState::new(
            None,
            Instant::now(),
            ahs_shared::RATE_LIMIT_PER_MIN,
            Arc::new(Identity::generate()),
        )
    }

    // The warn-only RSS leak signal: a threshold the live process already
    // exceeds (1 MiB) must fire exactly once and latch, never exit or mutate
    // anything else. A `0` threshold disables it entirely.
    #[test]
    fn rss_warn_fires_once_then_latches() {
        let (tx, mut rx) = unbounded_channel();
        let out = output::Output::capture(tx);
        let mut state = fresh_state();

        // Real RSS is tens of MiB ≫ 1, so this crosses immediately.
        warn_on_high_rss(&mut state, &out, 1);
        assert!(state.rss_warned, "first crossing latches the warn");
        let first = rx.try_recv();
        assert!(first.is_ok(), "an info event is emitted on the crossing");

        // Second tick: latched, so no second event (no per-tick spam).
        warn_on_high_rss(&mut state, &out, 1);
        assert!(
            rx.try_recv().is_err(),
            "warn fires exactly once per process"
        );
    }

    #[test]
    fn rss_warn_disabled_by_zero_threshold() {
        let (tx, mut rx) = unbounded_channel();
        let out = output::Output::capture(tx);
        let mut state = fresh_state();
        warn_on_high_rss(&mut state, &out, 0);
        assert!(!state.rss_warned, "threshold 0 disables the warn");
        assert!(rx.try_recv().is_err(), "no event when disabled");
    }
}
