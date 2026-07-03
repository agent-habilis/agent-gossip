//! The **membership layer**: pure roster transitions. Decides what an
//! inbound message means for the participant roster (`participants` /
//! `quiet`) — first-sight, return-from-quiet, departure — without any
//! transport, presentation, or surfacing concern. See the Concept
//! Glossary in AGENTS.md for the layer split.

use crate::daemon::state::EventLoopState;
use crate::protocol::{MessageKind, Nickname, PresenceSubtype};

/// Summary of how a newly received message changes our view of the
/// sender's membership. Computed from the message kind + current state
/// as a pure function; applied afterwards by the caller.
pub(crate) struct MembershipUpdate {
    /// The sender was in `quiet` (evicted as silent) and this
    /// message proves they're back. Triggers `peer_return` output and
    /// re-inclusion in `participants`.
    pub returned: bool,
    /// This is the first time we've seen this author. Gates the
    /// state-file write and the `has joined` line (we suppress it
    /// when `returned` — we print `came back` instead).
    pub joined_new: bool,
}

/// Decide what a message from `author` means for our participant
/// tracking. Pure: takes `&` refs, no side effects.
///
/// `Left` never counts as a join (even on first sight), because a
/// farewell is not an arrival. Everything else (including `Alive`,
/// `Msg`, `PeerInfo`) participates in self-heal: if we
/// haven't seen the peer before, we treat the message as proof of
/// presence.
pub(crate) fn compute(
    kind: &MessageKind,
    author: &Nickname,
    state: &EventLoopState,
) -> MembershipUpdate {
    let returned = state.quiet.contains(author.as_str());
    let joined_new = match kind {
        // A `Left` is explicit departure; durable state events never mark a
        // peer present either — anti-entropy backfill replays historical state
        // whose author may have long since left, and resurrecting them would
        // forge membership.
        MessageKind::Presence {
            subtype: PresenceSubtype::Left,
        }
        | MessageKind::State
        | MessageKind::Meta => false,
        MessageKind::Msg { .. }
        | MessageKind::Notice { .. }
        | MessageKind::Task { .. }
        | MessageKind::Presence {
            subtype: PresenceSubtype::Joined | PresenceSubtype::Alive,
        }
        | MessageKind::PeerInfo
        | MessageKind::Digest
        | MessageKind::StateDigest
        | MessageKind::MetaDigest
        | MessageKind::Ping
        | MessageKind::Pong { .. } => !state.participants.contains(author.as_str()),
    };
    MembershipUpdate {
        returned,
        joined_new,
    }
}

/// Apply a membership update to the mutable state. Mirror of the
/// pure `compute`: all side effects live here so tests of `compute`
/// remain free of any mutation.
pub(crate) fn apply(update: &MembershipUpdate, author: &Nickname, state: &mut EventLoopState) {
    if update.returned {
        state.quiet.remove(author.as_str());
        state.participants.insert(author.clone());
    }
    if update.joined_new {
        state.participants.insert(author.clone());
    }
    if update.returned || update.joined_new {
        state.write_participant_count();
        tracing::debug!(
            nickname = %author,
            returned = update.returned,
            joined_new = update.joined_new,
            participants = state.participants.len(),
            "roster changed"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::{
        EventLoopState, MembershipUpdate, MessageKind, Nickname, PresenceSubtype, apply, compute,
    };

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
    fn first_time_seeing_author_is_joined_new() {
        let state = fresh_state();
        let update = compute(
            &MessageKind::Msg { reply: None },
            &nick("swift-cedar"),
            &state,
        );
        assert!(update.joined_new);
        assert!(!update.returned);
    }

    #[test]
    fn known_author_is_not_joined_new() {
        let mut state = fresh_state();
        state.participants.insert(nick("swift-cedar"));
        let update = compute(
            &MessageKind::Msg { reply: None },
            &nick("swift-cedar"),
            &state,
        );
        assert!(!update.joined_new);
        assert!(!update.returned);
    }

    #[test]
    fn left_is_never_joined_new() {
        let state = fresh_state();
        let update = compute(
            &MessageKind::Presence {
                subtype: PresenceSubtype::Left,
            },
            &nick("swift-cedar"),
            &state,
        );
        assert!(!update.joined_new);
    }

    #[test]
    fn quiet_peer_message_marks_returned() {
        let mut state = fresh_state();
        state.quiet.insert(nick("swift-cedar"));
        let update = compute(
            &MessageKind::Msg { reply: None },
            &nick("swift-cedar"),
            &state,
        );
        assert!(update.returned);
        assert!(update.joined_new); // not in participants yet
    }

    #[test]
    fn alive_from_unknown_author_is_joined_new() {
        // Regression: Alive keepalives must self-heal membership.
        let state = fresh_state();
        let update = compute(
            &MessageKind::Presence {
                subtype: PresenceSubtype::Alive,
            },
            &nick("swift-cedar"),
            &state,
        );
        assert!(update.joined_new);
    }

    #[test]
    fn apply_inserts_into_participants_on_joined_new() {
        let mut state = fresh_state();
        let update = MembershipUpdate {
            returned: false,
            joined_new: true,
        };
        apply(&update, &nick("swift-cedar"), &mut state);
        assert!(state.participants.contains("swift-cedar"));
    }

    #[test]
    fn apply_removes_from_quiet_on_returned() {
        let mut state = fresh_state();
        state.quiet.insert(nick("swift-cedar"));
        let update = MembershipUpdate {
            returned: true,
            joined_new: false,
        };
        apply(&update, &nick("swift-cedar"), &mut state);
        assert!(!state.quiet.contains("swift-cedar"));
        assert!(state.participants.contains("swift-cedar"));
    }
}
