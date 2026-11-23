use habilis_network::ops::{PendingSend, SendVerdict};
use tokio::sync::oneshot;

use crate::a2a::tuning;

/// The verdict of 1 send, decided 1 time.
///
/// The engine gives a raw handle ([`PendingSend`]) to a send that is not done yet. [`Decided::decide`]
/// is the only place that turns the raw handles of 1 send into its verdict, and the only place that
/// reads the clock for it: 1 deadline for all the handles of the send. The task that it spawns gives
/// the same value to every effect of the send (the sent flag of the shard cache, the record of an own
/// leg, the echo) and then to the caller. A layer above takes a `Decided` with [`Decided::follow`] and
/// has no deadline of its own, so nothing judges a value that another layer has judged. The CLI has no
/// clock on the answer of the daemon either (the IPC client reads the reply with no timeout), so this is
/// the only clock of a send.
#[derive(Debug)]
pub(crate) struct Decided(oneshot::Receiver<SendVerdict>);

impl Decided {
    /// Decide the verdict of the raw handles of 1 send. The first verdict that is not `Delivered` is the
    /// verdict. A handle with no outcome by the deadline, or whose task ended with none, is
    /// `Unconfirmed`: the node does not know whether the peer took the frame. `effect` runs in the task,
    /// with the verdict, before the caller hears it.
    pub(crate) fn decide(
        raw: Vec<PendingSend>,
        effect: impl FnOnce(&SendVerdict) + Send + 'static,
    ) -> Self {
        let deadline = tokio::time::Instant::now() + tuning::send_backstop();
        let (verdict_tx, verdict_rx) = oneshot::channel();
        tokio::spawn(async move {
            let verdict = settle(raw, deadline).await;
            effect(&verdict);
            let _ = verdict_tx.send(verdict);
        });
        Self(verdict_rx)
    }

    /// A layer above the decision: `effect` runs with the verdict that `decided` gives, and the caller
    /// hears the same verdict after it. There is no deadline here: `decide` answers by its own.
    pub(crate) fn follow(
        decided: Vec<Self>,
        effect: impl FnOnce(&SendVerdict) + Send + 'static,
    ) -> Self {
        let (verdict_tx, verdict_rx) = oneshot::channel();
        tokio::spawn(async move {
            let verdict = first_verdict(decided).await;
            effect(&verdict);
            let _ = verdict_tx.send(verdict);
        });
        Self(verdict_rx)
    }
}

/// The first verdict that is not `Delivered` of the sends that a layer holds, or `Delivered` when every
/// one is. A task that ended with no verdict is `Unconfirmed`.
pub(crate) async fn first_verdict(decided: Vec<Decided>) -> SendVerdict {
    for send in decided {
        let verdict = send.0.await.unwrap_or_else(|_| {
            SendVerdict::Unconfirmed("a send task ended before it gave its outcome".to_owned())
        });
        if verdict != SendVerdict::Delivered {
            return verdict;
        }
    }
    SendVerdict::Delivered
}

/// The raw handles of 1 send, read against 1 deadline. `timeout_at` polls a handle before it reads the
/// clock, so a verdict that is ready at the deadline is the verdict.
async fn settle(raw: Vec<PendingSend>, deadline: tokio::time::Instant) -> SendVerdict {
    for handle in raw {
        let verdict = match tokio::time::timeout_at(deadline, handle).await {
            Ok(Ok(verdict)) => verdict,
            Ok(Err(_)) => {
                SendVerdict::Unconfirmed("a send task ended before it gave its outcome".to_owned())
            }
            Err(_) => SendVerdict::Unconfirmed("a send gave no outcome by its deadline".to_owned()),
        };
        if verdict != SendVerdict::Delivered {
            return verdict;
        }
    }
    SendVerdict::Delivered
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use habilis_network::ops::{LONGEST_SEND_CALL, SendVerdict};
    use tokio::sync::oneshot;

    use super::{Decided, first_verdict};
    use crate::a2a::tuning;

    /// A layer above the decision has no deadline: when the raw handle never answers, the only
    /// verdict is the one that `decide` gives at its own deadline, and the layer passes it on.
    #[tokio::test(start_paused = true)]
    async fn a_layer_above_the_decision_has_no_deadline_of_its_own() {
        let (_held, silent) = oneshot::channel::<SendVerdict>();
        let decided = Decided::decide(vec![silent], |_| {});
        let above = Decided::follow(vec![decided], |_| {});

        tokio::time::advance(tuning::send_backstop() + Duration::from_secs(1)).await;

        assert_eq!(
            first_verdict(vec![above]).await,
            SendVerdict::Unconfirmed("a send gave no outcome by its deadline".to_owned()),
            "the verdict of the layer above is the verdict that decide gave, with the text of decide"
        );
    }

    /// The backstop is a net under the engine, not a second decider: it is above the longest send call
    /// that the engine promises, so the engine answers first, and it is built from that constant, so the
    /// 2 cannot drift. It does not read the task timeout (`--task-timeout-secs`).
    #[test]
    fn the_backstop_is_above_the_longest_send_call_of_the_engine() {
        assert!(
            tuning::send_backstop() > LONGEST_SEND_CALL,
            "a backstop at or under the engine bound would answer for the engine"
        );
    }

    /// The effect of a decision and the caller get the same value, from the same task.
    #[tokio::test]
    async fn the_effect_and_the_caller_hear_the_same_verdict() {
        let (verdict_tx, raw) = oneshot::channel::<SendVerdict>();
        let (seen_tx, seen_rx) = oneshot::channel::<SendVerdict>();
        let decided = Decided::decide(vec![raw], move |verdict| {
            let _ = seen_tx.send(verdict.clone());
        });

        verdict_tx
            .send(SendVerdict::NotDelivered("no dial".to_owned()))
            .expect("the decision holds the receiver");

        assert_eq!(
            first_verdict(vec![decided]).await,
            SendVerdict::NotDelivered("no dial".to_owned()),
            "the caller hears the verdict of the handle"
        );
        assert_eq!(
            seen_rx
                .await
                .expect("the effect ran before the caller heard"),
            SendVerdict::NotDelivered("no dial".to_owned()),
            "the effect saw the same verdict"
        );
    }

    /// A task that ended with no verdict leaves it unknown whether the peer has the frame.
    #[tokio::test]
    async fn a_decision_whose_handle_was_dropped_is_not_confirmed() {
        let (verdict_tx, raw) = oneshot::channel::<SendVerdict>();
        drop(verdict_tx);

        let verdict = first_verdict(vec![Decided::decide(vec![raw], |_| {})]).await;

        assert!(
            matches!(verdict, SendVerdict::Unconfirmed(_)),
            "a handle that closed with no verdict is {verdict:?}"
        );
    }
}
