//! Heartbeat: the `Alive` keepalive emitter and the silence sweep
//! that evicts participants we've stopped hearing from. Part of the
//! lifecycle subsystem (it drives `peer_timeout` / the roster).

use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh_gossip::api::GossipSender;

use crate::daemon::state::EventLoopState;
use crate::output;
use crate::protocol::{Message, Nickname, SwarmId};
use crate::util::tuning::{ALIVE_INTERVAL_SECS, alive_timeout_secs};

/// Emit an `Alive` keepalive if we haven't broadcast anything
/// recently. Chatty daemons pay zero heartbeat cost.
pub(crate) async fn tick_alive(
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
) {
    if state.last_sent_at.elapsed() < Duration::from_secs(ALIVE_INTERVAL_SECS) {
        return;
    }
    let msg = Message::new_alive(swarm, author).signed(&state.identity);
    if let Ok(bytes) = msg.serialize() {
        let _ = sender.broadcast(Bytes::from(bytes)).await;
    }
    state.last_sent_at = Instant::now();
    tracing::trace!("alive keepalive broadcast");
}

/// Sweep `last_seen` for participants we've not heard from past the
/// timeout. Each eviction removes them from `last_seen`/`participants`
/// and rewrites the statusline. A participant whose arrival we
/// *surfaced* is also inserted into `quiet` and emits `peer_timeout`;
/// a ghost known only through pre-join anti-entropy backlog is evicted
/// silently (never surfaced as arriving, so never surfaced as
/// leaving) — keeps the join-horizon view symmetric.
pub(crate) fn tick_sweep(state: &mut EventLoopState, out: &output::Output) {
    let now = Instant::now();
    let timeout = Duration::from_secs(alive_timeout_secs());
    let expired: Vec<(Nickname, Instant, u64)> = state
        .last_seen
        .iter()
        .filter_map(|(nick, seen)| {
            let age = now.duration_since(*seen);
            (age > timeout).then(|| (nick.clone(), *seen, age.as_secs()))
        })
        .collect();
    for (nick, seen, age) in expired {
        state.last_seen.remove(nick.as_str());
        state.participant_endpoints.remove(nick.as_str());
        state.participant_meta.remove(nick.as_str());
        if state.participants.remove(nick.as_str()) {
            state.write_participant_count();
            if state.surfaced.remove(nick.as_str()) {
                state.quiet.insert(nick.clone());
                // Retain the last-heard instant so the roster can still
                // report this evictee's recency (its `last_seen` is gone).
                state.quiet_since.insert(nick.clone(), seen);
                out.peer_timeout(&nick, age);
                tracing::debug!(nickname = %nick, age_secs = age, "peer evicted (silence timeout)");
            } else {
                tracing::trace!(
                    nickname = %nick,
                    age_secs = age,
                    "ghost evicted silently (pre-join backlog, never surfaced)"
                );
            }
        }
    }
    // Keep `quiet_since` bounded to current `quiet` membership: peers that
    // returned (drained from `quiet`) or fell off the bounded `quiet` FIFO
    // drop their stale recency here.
    let quiet = &state.quiet;
    state
        .quiet_since
        .retain(|nick, _| quiet.contains(nick.as_str()));
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{Duration, EventLoopState, Instant, Nickname, output, tick_sweep};
    use crate::util::tuning::alive_timeout_secs;

    fn fresh_state() -> EventLoopState {
        EventLoopState::new(
            None,
            Instant::now(),
            std::sync::Arc::new(crate::protocol::identity::Identity::generate()),
        )
    }

    fn nick(name: &str) -> Nickname {
        Nickname::from(name)
    }

    #[test]
    fn sweep_evicts_surfaced_participant_into_quiet() {
        let mut state = fresh_state();
        let expired_at = Instant::now()
            .checked_sub(Duration::from_secs(alive_timeout_secs() + 10))
            .unwrap();
        state.last_seen.insert(nick("swift-cedar"), expired_at);
        state.participants.insert(nick("swift-cedar"));
        // Arrival was surfaced => departure must be too.
        state.surfaced.insert(nick("swift-cedar"));

        tick_sweep(&mut state, &output::Output::silent());

        assert!(!state.last_seen.contains_key("swift-cedar"));
        assert!(!state.participants.contains("swift-cedar"));
        assert!(state.quiet.contains("swift-cedar"));
        assert!(!state.surfaced.contains("swift-cedar"));
    }

    #[test]
    fn quiet_peer_reports_real_recency_in_roster() {
        // Regression: a quiet peer must still report how long ago it was
        // last heard (not `null`), even though eviction drops `last_seen`.
        let mut state = fresh_state();
        let expired_at = Instant::now()
            .checked_sub(Duration::from_secs(alive_timeout_secs() + 10))
            .unwrap();
        state.last_seen.insert(nick("calm-otter"), expired_at);
        state.participants.insert(nick("calm-otter"));
        state.surfaced.insert(nick("calm-otter"));

        tick_sweep(&mut state, &output::Output::silent());

        let roster = state.roster_snapshot();
        let entry = roster
            .participants
            .iter()
            .find(|entry| entry.nickname.as_str() == "calm-otter")
            .expect("quiet peer present in roster");
        assert!(entry.quiet, "evicted peer is marked quiet");
        let secs = entry
            .last_seen_secs_ago
            .expect("quiet peer reports real recency, not null");
        assert!(
            secs >= alive_timeout_secs(),
            "recency reflects the actual silence age, got {secs}s"
        );
    }

    #[test]
    fn sweep_evicts_unsurfaced_participant_silently() {
        let mut state = fresh_state();
        let expired_at = Instant::now()
            .checked_sub(Duration::from_secs(alive_timeout_secs() + 10))
            .unwrap();
        state.last_seen.insert(nick("ghost-elm"), expired_at);
        state.participants.insert(nick("ghost-elm"));
        // Never in `surfaced`: known only via pre-join backlog.

        tick_sweep(&mut state, &output::Output::silent());

        // Still evicted from the roster (hygiene preserved)...
        assert!(!state.last_seen.contains_key("ghost-elm"));
        assert!(!state.participants.contains("ghost-elm"));
        // ...but never parked in `quiet` => no `went quiet` emitted.
        assert!(!state.quiet.contains("ghost-elm"));
    }

    #[test]
    fn sweep_keeps_recent_participant() {
        let mut state = fresh_state();
        state.last_seen.insert(nick("swift-cedar"), Instant::now());
        state.participants.insert(nick("swift-cedar"));

        tick_sweep(&mut state, &output::Output::silent());

        assert!(state.last_seen.contains_key("swift-cedar"));
        assert!(state.participants.contains("swift-cedar"));
        assert!(!state.quiet.contains("swift-cedar"));
    }

    #[test]
    fn sweep_noop_on_empty_last_seen() {
        let mut state = fresh_state();
        tick_sweep(&mut state, &output::Output::silent());
        assert!(state.participants.is_empty());
        assert!(state.quiet.is_empty());
    }

    #[test]
    fn sweep_preserves_other_participants() {
        let mut state = fresh_state();
        let expired_at = Instant::now()
            .checked_sub(Duration::from_secs(alive_timeout_secs() + 10))
            .unwrap();
        state.last_seen.insert(nick("stale-nick"), expired_at);
        state.participants.insert(nick("stale-nick"));
        state.last_seen.insert(nick("fresh-nick"), Instant::now());
        state.participants.insert(nick("fresh-nick"));

        tick_sweep(&mut state, &output::Output::silent());

        let expected_participants: HashSet<Nickname> = [nick("fresh-nick")].into_iter().collect();
        assert_eq!(state.participants, expected_participants);
    }
}
