//! Heartbeat: the `Alive` keepalive emitter and the silence sweep
//! that evicts peers we've stopped hearing from. Part of the
//! lifecycle subsystem (it drives `peer_timeout` / the roster).

use std::time::{Duration, Instant};

use crate::transport::MeshSender;
use bytes::Bytes;

use crate::daemon::state::EventLoopState;
use crate::gossip::event::{NodeEvent, NodeSink};
use crate::protocol::{MeshId, Message, Nickname};
use crate::util::tuning::{ALIVE_INTERVAL_SECS, alive_timeout_secs};

/// Emit an `Alive` keepalive if we haven't broadcast anything
/// recently. Chatty daemons pay zero heartbeat cost.
pub(crate) async fn tick_alive(
    state: &mut EventLoopState,
    sender: &MeshSender,
    mesh: &MeshId,
    author: &Nickname,
) {
    if state.last_sent_at.elapsed() < Duration::from_secs(ALIVE_INTERVAL_SECS) {
        return;
    }
    let msg = Message::new_alive(mesh, author).signed(&state.identity);
    if let Ok(bytes) = msg.serialize() {
        let _ = sender.broadcast(Bytes::from(bytes)).await;
    }
    state.last_sent_at = Instant::now();
    tracing::trace!("alive keepalive broadcast");
}

/// Sweep `last_seen` for peers we've not heard from past the
/// timeout. Each eviction removes them from `last_seen`/`peers`
/// and rewrites the statusline. A peer whose arrival we
/// *surfaced* is also inserted into `quiet` and emits `peer_timeout`;
/// a ghost known only through pre-join anti-entropy backlog is evicted
/// silently (never surfaced as arriving, so never surfaced as
/// leaving) — keeps the join-horizon view symmetric.
pub(crate) fn tick_sweep(state: &mut EventLoopState, sink: &dyn NodeSink) {
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
        if state.peers.remove(nick.as_str()) {
            state.write_peer_count();
            if state.surfaced.remove(nick.as_str()) {
                state.quiet.insert(nick.clone());
                // Retain the last-heard instant so the roster can still
                // report this evictee's recency (its `last_seen` is gone).
                // The endpoint binding is retained too: a quiet peer is the
                // one expected to *return*, and its return (any signed
                // message) re-adds it to the roster without a `PeerInfo`
                // re-broadcast — dropping the binding here would leave a
                // returned peer undeliverable for directed frames until its
                // gossip link happens to bounce.
                state.quiet_since.insert(nick.clone(), seen);
                sink.emit(NodeEvent::PeerTimeout {
                    nickname: nick.clone(),
                    last_seen_secs_ago: age,
                });
                tracing::debug!(nickname = %nick, age_secs = age, "peer evicted (silence timeout)");
            } else {
                state.peer_endpoints.remove(nick.as_str());
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
    // Same bound for the endpoint dial hints: an entry lives as long as its
    // nick is an active peer or a returnable quiet evictee, so the map
    // is capped at |peers| + QUIET_CAP even under nickname churn.
    // (Safe against a just-arrived `PeerInfo`: `lifecycle::observe` runs
    // before `handle_peer_info`, so a recorded endpoint's author is already a
    // peer.)
    let peers = &state.peers;
    state
        .peer_endpoints
        .retain(|nick, _| peers.contains(nick) || quiet.contains(nick.as_str()));
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{Duration, EventLoopState, Instant, Nickname, tick_sweep};
    use crate::gossip::event::SilentSink;
    use crate::util::tuning::alive_timeout_secs;

    fn fresh_state() -> EventLoopState {
        EventLoopState::new(
            crate::daemon::state::StateInit {
                state_file: None,
                identity: std::sync::Arc::new(crate::protocol::identity::Identity::generate()),
                secrets: crate::daemon::state::MeshSecrets::default(),
                per_peer_gate: None,
            },
            Instant::now(),
        )
    }

    fn nick(name: &str) -> Nickname {
        Nickname::from(name)
    }

    #[test]
    fn sweep_evicts_surfaced_peer_into_quiet() {
        let mut state = fresh_state();
        let expired_at = Instant::now()
            .checked_sub(Duration::from_secs(alive_timeout_secs() + 10))
            .unwrap();
        state.last_seen.insert(nick("swift-cedar"), expired_at);
        state.peers.insert(nick("swift-cedar"));
        // Arrival was surfaced => departure must be too.
        state.surfaced.insert(nick("swift-cedar"));

        tick_sweep(&mut state, &SilentSink);

        assert!(!state.last_seen.contains_key("swift-cedar"));
        assert!(!state.peers.contains("swift-cedar"));
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
        state.peers.insert(nick("calm-otter"));
        state.surfaced.insert(nick("calm-otter"));

        tick_sweep(&mut state, &SilentSink);

        let roster = state.roster_snapshot();
        let entry = roster
            .peers
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
    fn sweep_keeps_a_quiet_peers_endpoint_for_its_return() {
        let mut state = fresh_state();
        let expired_at = Instant::now()
            .checked_sub(Duration::from_secs(alive_timeout_secs() + 10))
            .unwrap();
        state.last_seen.insert(nick("swift-cedar"), expired_at);
        state.peers.insert(nick("swift-cedar"));
        state.surfaced.insert(nick("swift-cedar"));
        let endpoint = iroh::SecretKey::from_bytes(&[7; 32]).public();
        state.peer_endpoints.insert(nick("swift-cedar"), endpoint);

        tick_sweep(&mut state, &SilentSink);

        assert!(state.quiet.contains("swift-cedar"));
        assert_eq!(
            state.peer_endpoints.get("swift-cedar"),
            Some(&endpoint),
            "a returnable quiet peer keeps its dial hint — its return never re-broadcasts PeerInfo"
        );
    }

    #[test]
    fn sweep_prunes_endpoints_of_nicks_neither_active_nor_quiet() {
        let mut state = fresh_state();
        let endpoint = iroh::SecretKey::from_bytes(&[8; 32]).public();
        // Not in `peers`, not in `quiet`: fell off the quiet FIFO or
        // departed via `left` churn — the dial hint must not outlive it.
        state.peer_endpoints.insert(nick("gone-fern"), endpoint);

        tick_sweep(&mut state, &SilentSink);

        assert!(!state.peer_endpoints.contains_key("gone-fern"));
    }

    #[test]
    fn sweep_evicts_unsurfaced_peer_silently() {
        let mut state = fresh_state();
        let expired_at = Instant::now()
            .checked_sub(Duration::from_secs(alive_timeout_secs() + 10))
            .unwrap();
        state.last_seen.insert(nick("ghost-elm"), expired_at);
        state.peers.insert(nick("ghost-elm"));
        // Never in `surfaced`: known only via pre-join backlog.

        tick_sweep(&mut state, &SilentSink);

        // Still evicted from the roster (hygiene preserved)...
        assert!(!state.last_seen.contains_key("ghost-elm"));
        assert!(!state.peers.contains("ghost-elm"));
        // ...but never parked in `quiet` => no `went quiet` emitted.
        assert!(!state.quiet.contains("ghost-elm"));
    }

    #[test]
    fn sweep_keeps_recent_peer() {
        let mut state = fresh_state();
        state.last_seen.insert(nick("swift-cedar"), Instant::now());
        state.peers.insert(nick("swift-cedar"));

        tick_sweep(&mut state, &SilentSink);

        assert!(state.last_seen.contains_key("swift-cedar"));
        assert!(state.peers.contains("swift-cedar"));
        assert!(!state.quiet.contains("swift-cedar"));
    }

    #[test]
    fn sweep_noop_on_empty_last_seen() {
        let mut state = fresh_state();
        tick_sweep(&mut state, &SilentSink);
        assert!(state.peers.is_empty());
        assert!(state.quiet.is_empty());
    }

    #[test]
    fn sweep_preserves_other_peers() {
        let mut state = fresh_state();
        let expired_at = Instant::now()
            .checked_sub(Duration::from_secs(alive_timeout_secs() + 10))
            .unwrap();
        state.last_seen.insert(nick("stale-nick"), expired_at);
        state.peers.insert(nick("stale-nick"));
        state.last_seen.insert(nick("fresh-nick"), Instant::now());
        state.peers.insert(nick("fresh-nick"));

        tick_sweep(&mut state, &SilentSink);

        let expected_peers: HashSet<Nickname> = [nick("fresh-nick")].into_iter().collect();
        assert_eq!(state.peers, expected_peers);
    }
}
