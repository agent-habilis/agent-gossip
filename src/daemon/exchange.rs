//! The daemon-side **exchange** state machine + its two timers.
//!
//! An exchange is a directed, multi-leg conversation (see [`crate::protocol::MessageKind::Exchange`]);
//! `handover` is one behavior on it. The daemon owns only the *coarse*
//! lifecycle — phase advance, the per-exchange idle debounce, the ball-owner
//! keepalive, and the content-message cap — while the skill owns the
//! *content* (what to ask, whether to confirm, the brief, the plan).
//!
//! The machine is **distributed** with no consensus: each party derives
//! state from the legs it has seen, so the rules are deliberately
//! conservative — **monotonic** advance (a leg that would move backward is
//! ignored), **idempotent** (a duplicate leg is a no-op), and a terminal
//! record is frozen. Local triggers (timeout, cap) *broadcast* a terminal
//! `Cancel` so the other side converges.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh_gossip::api::GossipSender;

use crate::daemon::state::EventLoopState;
use crate::output;
use crate::protocol::{
    ExchangeId, ExchangeKind, ExchangePhase, Message, MessageBody, MessageKind, Nickname, SwarmId,
};
use crate::util::consts::EXCHANGE_CONTENT_CAP;
use crate::util::tuning::{exchange_keepalive_secs, exchange_timeout_secs};

/// My part in an exchange: did I open it, or receive the offer?
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ExchangeRole {
    Initiator,
    Receiver,
}

impl ExchangeRole {
    fn opposite(self) -> Self {
        match self {
            ExchangeRole::Initiator => ExchangeRole::Receiver,
            ExchangeRole::Receiver => ExchangeRole::Initiator,
        }
    }
}

/// The coarse lifecycle the daemon tracks. Ordered so a backward
/// transition is a cheap comparison: `Proposed < Active < Review < Terminal`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum ExchangeState {
    Proposed,
    Active,
    Review,
    Terminal,
}

/// One in-flight exchange this node is a party to.
#[derive(Clone, Debug)]
pub(crate) struct ExchangeRecord {
    /// The other party.
    pub peer: Nickname,
    pub kind: ExchangeKind,
    pub role: ExchangeRole,
    pub state: ExchangeState,
    /// Local-clock instant of the last leg (inbound or our own) — the
    /// debounce reads this, never the wire `ts` (which can skew).
    pub last_activity: Instant,
    /// Count of **content** legs seen (progress excluded) — the cap.
    pub content_count: u32,
    /// Last progress fraction the receiver reported, replayed on keepalives.
    pub last_fraction: Option<(u64, u64)>,
    /// Which role currently owes the next move — that party's daemon emits
    /// the keepalive, and that party's silence is what eventually times out.
    pub ball: ExchangeRole,
}

impl ExchangeRecord {
    /// Am I the ball-owner (so my daemon keepalives this exchange)?
    fn i_own_ball(&self) -> bool {
        self.ball == self.role
    }
}

/// The fields of an applied leg the state machine needs.
pub(crate) struct LegInfo<'a> {
    pub exchange_id: &'a ExchangeId,
    /// The other party (inbound: the author; outbound: the `to`).
    pub peer: &'a Nickname,
    pub kind: ExchangeKind,
    pub phase: ExchangePhase,
    /// `true` if we sent this leg (outbound echo), `false` if it arrived.
    pub mine: bool,
    /// Parsed `done/total` for a `Progress` leg (else `None`).
    pub fraction: Option<(u64, u64)>,
}

/// Parse a `Progress` body's `done/total` fraction (e.g. `"35/100"`);
/// `None` for an empty or unparseable beat (indeterminate progress).
pub(crate) fn parse_fraction(body: &str) -> Option<(u64, u64)> {
    let (done, total) = body.split_once('/')?;
    Some((done.trim().parse().ok()?, total.trim().parse().ok()?))
}

/// Feed one exchange leg into the registry from the broadcast (`mine =
/// true`) or receive (`mine = false`) path. The single call site for both:
/// it derives the peer (the `to` we sent to, or the `author` we heard from)
/// and the progress fraction, then advances the machine. A non-`Exchange`
/// message is a no-op. Returns `true` if this leg crossed the content cap.
pub(crate) fn ingest(
    exchanges: &mut HashMap<ExchangeId, ExchangeRecord>,
    msg: &Message,
    mine: bool,
    now: Instant,
) -> bool {
    let MessageKind::Exchange {
        to,
        exchange_id,
        kind,
        phase,
    } = &msg.kind
    else {
        return false;
    };
    let peer = if mine { to } else { &msg.author };
    let fraction = matches!(phase, ExchangePhase::Progress)
        .then(|| parse_fraction(msg.body.as_str()))
        .flatten();
    apply(
        exchanges,
        &LegInfo {
            exchange_id,
            peer,
            kind: *kind,
            phase: *phase,
            mine,
            fraction,
        },
        now,
    )
}

/// Apply one exchange leg to the registry, advancing the coarse machine.
/// Monotonic + idempotent + terminal-frozen. Returns `true` if this leg
/// pushed the content count **over** the cap (the daemon warns once).
pub(crate) fn apply(
    exchanges: &mut HashMap<ExchangeId, ExchangeRecord>,
    leg: &LegInfo<'_>,
    now: Instant,
) -> bool {
    // A terminal record is immutable — late/duplicate legs are ignored.
    if exchanges
        .get(leg.exchange_id)
        .is_some_and(|rec| rec.state == ExchangeState::Terminal)
    {
        return false;
    }

    if matches!(leg.phase, ExchangePhase::Offer) {
        // The opening leg mints the record; a duplicate offer just touches it.
        exchanges
            .entry(leg.exchange_id.clone())
            .or_insert_with(|| ExchangeRecord {
                peer: leg.peer.clone(),
                kind: leg.kind,
                role: if leg.mine {
                    ExchangeRole::Initiator
                } else {
                    ExchangeRole::Receiver
                },
                state: ExchangeState::Proposed,
                last_activity: now,
                content_count: 0,
                last_fraction: None,
                ball: ExchangeRole::Receiver,
            });
    }

    let Some(rec) = exchanges.get_mut(leg.exchange_id) else {
        // A non-offer leg for an exchange we never saw open — drop it (out of
        // order, or an exchange that began before our join horizon).
        return false;
    };

    advance(rec, leg.phase, leg.mine);
    if let Some(fraction) = leg.fraction {
        rec.last_fraction = Some(fraction);
    }
    rec.last_activity = now;

    // Content legs (everything but the plumbing `Progress`) burn the budget.
    let mut over_cap = false;
    if crate::protocol::message::is_content_phase(leg.phase) {
        rec.content_count = rec.content_count.saturating_add(1);
        over_cap = rec.content_count == EXCHANGE_CONTENT_CAP + 1;
    }
    over_cap
}

/// The per-phase coarse transition (state + ball). Illegal/out-of-order
/// phases for the current state are silently ignored (the conservative,
/// no-consensus rule).
fn advance(rec: &mut ExchangeRecord, phase: ExchangePhase, mine: bool) {
    let sender = if mine { rec.role } else { rec.role.opposite() };
    match phase {
        ExchangePhase::Accept if rec.state == ExchangeState::Proposed => {
            // The receiver commits and keeps the ball (it now does the work).
            rec.state = ExchangeState::Active;
            rec.ball = ExchangeRole::Receiver;
        }
        ExchangePhase::Context
            if matches!(rec.state, ExchangeState::Proposed | ExchangeState::Active) =>
        {
            // A question/answer: advance into the exchange, flip the ball to
            // whoever now owes the reply.
            rec.state = ExchangeState::Active;
            rec.ball = sender.opposite();
        }
        ExchangePhase::Done if rec.state == ExchangeState::Active => {
            // Request to close → the initiator reviews.
            rec.state = ExchangeState::Review;
            rec.ball = ExchangeRole::Initiator;
        }
        ExchangePhase::Change if rec.state == ExchangeState::Review => {
            // Reviewer wants revisions → back to the receiver.
            rec.state = ExchangeState::Active;
            rec.ball = ExchangeRole::Receiver;
        }
        ExchangePhase::Confirm if rec.state == ExchangeState::Review => {
            rec.state = ExchangeState::Terminal;
        }
        ExchangePhase::Decline
            if matches!(rec.state, ExchangeState::Proposed | ExchangeState::Active) =>
        {
            rec.state = ExchangeState::Terminal;
        }
        ExchangePhase::Cancel => rec.state = ExchangeState::Terminal,
        // No-op here: `Offer` is handled at record creation, `Progress` is
        // liveness-only, and every other variant lands here only when its
        // guard above failed (an out-of-order/backward leg, conservatively
        // ignored). Listed explicitly so a new phase forces a decision.
        ExchangePhase::Offer
        | ExchangePhase::Progress
        | ExchangePhase::Accept
        | ExchangePhase::Context
        | ExchangePhase::Done
        | ExchangePhase::Change
        | ExchangePhase::Confirm
        | ExchangePhase::Decline => {}
    }
}

/// Sweep the registry for exchanges idle past the debounce timeout, then
/// **garbage-collect** records that have been terminal longer than the
/// timeout. Each eviction freezes the record, emits a `exchange_timeout` event,
/// and broadcasts a terminal `Cancel` so the peer converges; the GC pass
/// keeps `state.exchanges` bounded (a terminal record older than the timeout is
/// past the dedup window — no further leg for that `exchange_id` will arrive, so
/// it is safe to drop). The exchange analogue of
/// [`crate::lifecycle::heartbeat::tick_sweep`].
pub(crate) async fn tick_exchange_sweep(
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    out: &output::Output,
) {
    let now = Instant::now();
    let timeout = Duration::from_secs(exchange_timeout_secs());
    let expired: Vec<(ExchangeId, Nickname, ExchangeKind)> = state
        .exchanges
        .iter()
        .filter(|(_, rec)| {
            rec.state != ExchangeState::Terminal && now.duration_since(rec.last_activity) > timeout
        })
        .map(|(exchange_id, rec)| (exchange_id.clone(), rec.peer.clone(), rec.kind))
        .collect();

    for (exchange_id, peer, kind) in expired {
        if let Some(rec) = state.exchanges.get_mut(&exchange_id) {
            rec.state = ExchangeState::Terminal;
        }
        out.exchange_timeout(&exchange_id);
        tracing::debug!(%exchange_id, %peer, "exchange evicted (idle-debounce timeout)");
        broadcast_leg(
            state,
            sender,
            swarm,
            author,
            &peer,
            &exchange_id,
            kind,
            ExchangePhase::Cancel,
            "timeout",
        )
        .await;
    }

    // GC: drop terminal records past the dedup window so the registry stays
    // bounded over a long-lived daemon-side exchange churn (the analogue of the
    // heartbeat sweep pruning `quiet_since`).
    state.exchanges.retain(|_, rec| {
        rec.state != ExchangeState::Terminal || now.duration_since(rec.last_activity) <= timeout
    });
}

/// Emit a `Progress` keepalive for every live exchange whose ball we hold and
/// that we've gone quiet on past the keepalive cadence — so a silent owner
/// (deciding, executing, or reviewing) does not wrongly time out. The exchange
/// analogue of [`crate::lifecycle::heartbeat::tick_alive`].
pub(crate) async fn tick_exchange_keepalive(
    state: &mut EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
) {
    /// `(exchange_id, peer, kind, last_fraction)` for one keepalive-due exchange.
    type KeepaliveDue = (ExchangeId, Nickname, ExchangeKind, Option<(u64, u64)>);

    let now = Instant::now();
    let interval = Duration::from_secs(exchange_keepalive_secs());
    let due: Vec<KeepaliveDue> = state
        .exchanges
        .iter()
        .filter(|(_, rec)| {
            rec.state != ExchangeState::Terminal
                && rec.i_own_ball()
                && now.duration_since(rec.last_activity) >= interval
        })
        .map(|(exchange_id, rec)| {
            (
                exchange_id.clone(),
                rec.peer.clone(),
                rec.kind,
                rec.last_fraction,
            )
        })
        .collect();

    for (exchange_id, peer, kind, fraction) in due {
        let body = match fraction {
            Some((done, total)) => format!("{done}/{total}"),
            None => String::new(),
        };
        broadcast_leg(
            state,
            sender,
            swarm,
            author,
            &peer,
            &exchange_id,
            kind,
            ExchangePhase::Progress,
            &body,
        )
        .await;
        if let Some(rec) = state.exchanges.get_mut(&exchange_id) {
            rec.last_activity = Instant::now();
        }
    }
}

/// Build, sign, and fire-and-forget a daemon-originated exchange leg (the
/// keepalive `Progress` and the timeout `Cancel`). Unsigned-body validation
/// can't fail for our short literals, so a serialize error is swallowed like
/// any other plumbing broadcast.
#[expect(clippy::too_many_arguments, reason = "a signed exchange leg's fields")]
async fn broadcast_leg(
    state: &EventLoopState,
    sender: &GossipSender,
    swarm: &SwarmId,
    author: &Nickname,
    peer: &Nickname,
    exchange_id: &ExchangeId,
    kind: ExchangeKind,
    phase: ExchangePhase,
    body: &str,
) {
    let Ok(body) = MessageBody::new(body) else {
        return;
    };
    let msg = Message::new_exchange(
        swarm,
        author,
        peer.clone(),
        exchange_id.clone(),
        kind,
        phase,
        body,
    )
    .signed(&state.identity);
    if let Ok(bytes) = msg.serialize() {
        let _ = sender.broadcast(Bytes::from(bytes)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{ExchangeRole, ExchangeState, LegInfo, apply};
    use crate::protocol::{ExchangeId, ExchangeKind, ExchangePhase, Nickname};
    use std::collections::HashMap;
    use std::time::Instant;

    fn tid() -> ExchangeId {
        ExchangeId::from("550e8400-e29b-41d4-a716-446655440000")
    }

    fn leg(phase: ExchangePhase, mine: bool) -> LegInfo<'static> {
        // Leak a fixed peer/exchange_id for 'static convenience in tests.
        LegInfo {
            exchange_id: Box::leak(Box::new(tid())),
            peer: Box::leak(Box::new(Nickname::from("calm-otter"))),
            kind: ExchangeKind::Handover,
            phase,
            mine,
            fraction: None,
        }
    }

    /// Drive the happy-path lifecycle from the initiator's view and assert
    /// the coarse state + ball-owner at each step.
    #[test]
    fn initiator_happy_path_states_and_ball() {
        let mut exchanges = HashMap::new();
        let now = Instant::now();

        apply(&mut exchanges, &leg(ExchangePhase::Offer, true), now);
        let rec = &exchanges[tid().as_str()];
        assert_eq!(rec.role, ExchangeRole::Initiator);
        assert_eq!(rec.state, ExchangeState::Proposed);
        assert_eq!(rec.ball, ExchangeRole::Receiver);

        apply(&mut exchanges, &leg(ExchangePhase::Accept, false), now);
        assert_eq!(exchanges[tid().as_str()].state, ExchangeState::Active);
        assert_eq!(exchanges[tid().as_str()].ball, ExchangeRole::Receiver);

        // Receiver asks a question → ball flips to me (the initiator).
        apply(&mut exchanges, &leg(ExchangePhase::Context, false), now);
        assert_eq!(exchanges[tid().as_str()].ball, ExchangeRole::Initiator);

        apply(&mut exchanges, &leg(ExchangePhase::Done, false), now);
        assert_eq!(exchanges[tid().as_str()].state, ExchangeState::Review);
        assert_eq!(exchanges[tid().as_str()].ball, ExchangeRole::Initiator);

        apply(&mut exchanges, &leg(ExchangePhase::Confirm, true), now);
        assert_eq!(exchanges[tid().as_str()].state, ExchangeState::Terminal);
    }

    /// A terminal exchange is frozen: a later leg never mutates it.
    #[test]
    fn terminal_is_frozen() {
        let mut exchanges = HashMap::new();
        let now = Instant::now();
        apply(&mut exchanges, &leg(ExchangePhase::Offer, true), now);
        apply(&mut exchanges, &leg(ExchangePhase::Decline, false), now);
        assert_eq!(exchanges[tid().as_str()].state, ExchangeState::Terminal);
        // A stray confirm after decline must not resurrect it.
        apply(&mut exchanges, &leg(ExchangePhase::Confirm, true), now);
        assert_eq!(exchanges[tid().as_str()].state, ExchangeState::Terminal);
    }

    /// A non-offer leg for an unknown exchange id is dropped (no record created).
    #[test]
    fn unknown_exchange_non_offer_is_ignored() {
        let mut exchanges = HashMap::new();
        apply(
            &mut exchanges,
            &leg(ExchangePhase::Context, false),
            Instant::now(),
        );
        assert!(exchanges.is_empty());
    }

    /// Out-of-order legs (a `Done` before `Accept`) are ignored, not applied.
    #[test]
    fn out_of_order_is_ignored() {
        let mut exchanges = HashMap::new();
        let now = Instant::now();
        apply(&mut exchanges, &leg(ExchangePhase::Offer, true), now);
        apply(&mut exchanges, &leg(ExchangePhase::Done, false), now);
        // Still Proposed — the premature Done did nothing.
        assert_eq!(exchanges[tid().as_str()].state, ExchangeState::Proposed);
    }

    /// Content legs count toward the cap; `Progress` does not, and the cap
    /// crossing is reported exactly once.
    #[test]
    fn content_cap_excludes_progress() {
        let mut exchanges = HashMap::new();
        let now = Instant::now();
        apply(&mut exchanges, &leg(ExchangePhase::Offer, true), now); // count 1
        apply(&mut exchanges, &leg(ExchangePhase::Accept, false), now); // count 2
        for _ in 0..50 {
            // Progress beats never increment the content count.
            apply(&mut exchanges, &leg(ExchangePhase::Progress, false), now);
        }
        assert_eq!(exchanges[tid().as_str()].content_count, 2);
        // Drive to exactly the cap, then one past it.
        let mut over = false;
        while exchanges[tid().as_str()].content_count < super::EXCHANGE_CONTENT_CAP {
            over |= apply(&mut exchanges, &leg(ExchangePhase::Context, false), now);
        }
        assert!(!over, "no crossing while at/under the cap");
        let crossing = apply(&mut exchanges, &leg(ExchangePhase::Context, false), now);
        assert!(crossing, "the leg that pushes past the cap reports it");
    }
}
