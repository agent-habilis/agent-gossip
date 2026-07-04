//! The relay leg of the lookup layer: the default **relay ladder** (n0
//! prod), how a [`RelayChoice`] maps to an iroh [`RelayMode`], and the
//! deterministic "home on the first reachable rung" bootstrap selection
//! plus its steady-state failover decision.
//!
//! Why a ladder (vs a single pinned relay, or iroh's latency-pick over
//! the set): the cold-join bootstrap needs every member to meet at the
//! *same* relay, and iroh does not reliably race multiple relay
//! candidates in an `EndpointAddr`. So members agree on a fixed order
//! and home on the first reachable rung — under equal relay visibility
//! that is a global function, so all converge on the same rung and fail
//! over together. The public analog of the private loopback-port ladder
//! (`beacon::RendezvousParams::bind_ports`).

use std::future::Future;
use std::sync::LazyLock;
use std::time::Duration;

use iroh::{
    Endpoint, RelayMode, RelayUrl,
    endpoint::{default_relay_mode, presets},
};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::protocol::swarm::RelayChoice;
use crate::util::tuning::{
    RELAY_LIVENESS_FAILS_TO_EVICT, RELAY_LIVENESS_INTERVAL_SECS, RELAY_REPROBE_BACKOFF_MAX_SECS,
    RELAY_REPROBE_BACKOFF_MIN_SECS, RELAY_RUNG_PROBE_SECS,
};

/// The default relay **ladder** — n0's prod relay set, in fixed
/// preference order. The beacon homes on the first reachable rung
/// ([`select_bootstrap_rung`]) and the joiner pre-registers
/// `rendezvous_id` at that same rung — `EndpointAddr::new(rendezvous_id)
/// .with_relay_url(rung)` — for a **zero-address-lookup** relay-direct
/// dial: a creator-independent bootstrap that needs no address lookup to
/// reach the rendezvous.
///
/// rung 0 is our own operated relay; rungs 1..4 are iroh `defaults::prod`
/// (NA-east, NA-west, EU, AP) as the resilient fallback. Hardcoded,
/// **not** `defaults::prod::default_relay_map()`, so members on different
/// iroh versions still agree on the *same* ladder. Relay infra is
/// versioned and not cross-compatible (sendme #121): an iroh bump can
/// move `defaults::prod` off these hosts, silently breaking the
/// relay-direct bootstrap for members on a different iroh version.
/// `pinned_ladder_matches_iroh_prod` is the tripwire — it fails
/// on such a move so we review (do these hosts still operate?) before
/// shipping the bump. See docs/iroh-ecosystem-research.md.
const RENDEZVOUS_RELAY_LADDER: [&str; 5] = [
    "https://swarm-relay.agent-habilis.com./",         // ours (rung 0)
    "https://use1-1.relay.n0.iroh-canary.iroh.link./", // NA-east
    "https://usw1-1.relay.n0.iroh-canary.iroh.link./", // NA-west
    "https://euc1-1.relay.n0.iroh-canary.iroh.link./", // EU
    "https://aps1-1.relay.n0.iroh-canary.iroh.link./", // AP
];

/// Parsed once; `RelayUrl` clones are `Arc`-backed (cheap).
static RENDEZVOUS_RELAY_LADDER_URLS: LazyLock<Vec<RelayUrl>> = LazyLock::new(|| {
    RENDEZVOUS_RELAY_LADDER
        .iter()
        .map(|raw| {
            raw.parse()
                .expect("RENDEZVOUS_RELAY_LADDER entries are valid relay URL constants")
        })
        .collect()
});

/// Resolve a [`RelayChoice`] into the ordered relay **ladder**:
/// `Disabled` ⇒ empty (no relay), `Pinned` ⇒ the default n0 prod
/// ladder, `Custom` ⇒ the operator ladder (`--relay a,b,c`, order
/// preserved). The beacon homes on, and the joiner pre-registers at, the
/// first reachable rung ([`select_bootstrap_rung`]) — the beacon leg of
/// [`relay_mode`] and `daemon::setup::register_rendezvous` must agree or
/// the relay-direct bootstrap dial finds nothing.
pub(crate) fn relay_ladder(choice: &RelayChoice) -> Vec<RelayUrl> {
    match choice {
        RelayChoice::Disabled => Vec::new(),
        RelayChoice::Pinned => RENDEZVOUS_RELAY_LADDER_URLS.clone(),
        RelayChoice::Custom(ladder) => ladder.clone(),
    }
}

/// Map a [`RelayChoice`] to the iroh [`RelayMode`] for an endpoint.
///
/// - **Beacon** homes on exactly **one** rung — the deterministic relay
///   joiners pre-register. The caller encodes that by passing a
///   one-element `Custom([rung])` (the rung `select_bootstrap_rung`
///   chose), so the beacon never reaches the multi-relay `Pinned` arm.
/// - **Participant** on `Pinned` uses iroh's resilient multi-relay
///   `default_relay_mode()` (the n0 prod set): pinning a participant to
///   one relay made `bind()` block on that relay's handshake and dropped
///   iroh's relay fallback — a `Default` participant still reaches the
///   beacon at its rung and skips relays entirely same-LAN via mDNS. A
///   `Custom` ladder pins the participant to that whole set.
///
/// So `Custom(ladder)` covers both: a 1-rung beacon and a multi-rung
/// participant alike map to `RelayMode::Custom`.
pub(super) fn relay_mode(choice: &RelayChoice) -> RelayMode {
    match choice {
        RelayChoice::Disabled => {
            tracing::debug!("relay disabled (not in the lookup allowlist)");
            RelayMode::Disabled
        }
        RelayChoice::Custom(ladder) => {
            tracing::debug!(rungs = ladder.len(), "endpoint pinned to relay ladder");
            RelayMode::custom(ladder.iter().cloned())
        }
        RelayChoice::Pinned => {
            tracing::debug!("participant on iroh resilient multi-relay default");
            default_relay_mode()
        }
    }
}

/// Walk a relay ladder in order and return the first **reachable** rung
/// — the deterministic bootstrap relay every member converges on under
/// equal visibility. Reachability is "an endpoint pinned to this single
/// relay reaches `online()` within `per_rung`": iroh pings the relay and
/// only reports online once a home relay is chosen, so a successful
/// `online()` against a one-relay `RelayMode` *is* that rung being live.
///
/// `None` ⇒ the ladder is empty (relay disabled) or every rung was
/// unreachable; the caller then leans on mDNS/DHT (the other reliability
/// layers).
pub(crate) async fn select_bootstrap_rung(
    ladder: &[RelayUrl],
    per_rung: Duration,
) -> Option<RelayUrl> {
    let selected = select_first_reachable(ladder, |rung| async move {
        let reachable = relay_rung_reachable(&rung, per_rung).await;
        if !reachable {
            tracing::debug!(target: "agent_habilis_swarm::lookup", relay = %rung, "bootstrap relay rung unreachable; trying next");
        }
        reachable
    })
    .await;
    match &selected {
        Some(rung) => {
            tracing::info!(target: "agent_habilis_swarm::lookup", relay = %rung, "selected bootstrap relay rung");
        }
        None if !ladder.is_empty() => {
            tracing::info!(target: "agent_habilis_swarm::lookup", rungs = ladder.len(), "no relay ladder rung reachable; relying on mDNS/DHT");
        }
        None => {}
    }
    selected
}

/// The pure ladder walk behind [`select_bootstrap_rung`]: probe rungs
/// **in order from the first** and return the first for which `reachable`
/// is `true`, else `None`. Always restarting at index 0 is what makes
/// the selection deterministic across members and gives "reset the
/// ladder and try again from the first rung" on every (re)selection — a
/// recovered earlier rung is preferred over a later one. Generic over
/// the reachability probe so the ordering/short-circuit logic is unit
/// tested without touching the network.
async fn select_first_reachable<Probe, Fut>(
    ladder: &[RelayUrl],
    mut reachable: Probe,
) -> Option<RelayUrl>
where
    Probe: FnMut(RelayUrl) -> Fut,
    Fut: Future<Output = bool>,
{
    for rung in ladder {
        if reachable(rung.clone()).await {
            return Some(rung.clone());
        }
    }
    None
}

/// Probe every rung of `ladder` for reachability, in order, returning each
/// rung's online status. Unlike [`select_bootstrap_rung`] this does **not**
/// short-circuit at the first reachable rung — `doctor --swarm` shows the full
/// ladder's health, not just the bootstrap pick.
pub(crate) async fn probe_ladder(ladder: &[RelayUrl], per_rung: Duration) -> Vec<(RelayUrl, bool)> {
    let mut statuses = Vec::with_capacity(ladder.len());
    for rung in ladder {
        let reachable = relay_rung_reachable(rung, per_rung).await;
        statuses.push((rung.clone(), reachable));
    }
    statuses
}

/// Probe a single relay rung: bind an ephemeral endpoint pinned to just
/// this relay and wait for `online()` within `timeout`. Closes the probe
/// endpoint before returning.
async fn relay_rung_reachable(rung: &RelayUrl, timeout: Duration) -> bool {
    let Ok(endpoint) = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::custom([rung.clone()]))
        .bind()
        .await
    else {
        return false;
    };
    let reachable = tokio::time::timeout(timeout, endpoint.online())
        .await
        .is_ok();
    endpoint.close().await;
    reachable
}

/// The outcome of comparing a freshly-selected rung against the current
/// one — applied by the event loop's rung-update arm when the beacon's
/// self-monitor publishes a new rung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RungRefresh {
    /// Keep the current rung (still the first reachable, or we must not
    /// act — not the beacon, or relay disabled).
    NoChange,
    /// Re-home the beacon onto this rung (the current one died, or an
    /// earlier preferred one recovered). `None` ⇒ no rung reachable;
    /// drop to mDNS/DHT.
    Rehome(Option<RelayUrl>),
}

/// Compare the freshly-selected rung against the one we are homed on.
/// Pure (no IO, no beacon/relay-enabled gating — that is the caller's
/// concern) so the change-detection policy is unit-tested directly:
/// `Rehome` iff the rung differs (the current one died, an earlier
/// preferred one recovered, or every rung is now down → `Rehome(None)`).
pub(crate) fn plan_rung_refresh(
    current: Option<&RelayUrl>,
    selected: Option<RelayUrl>,
) -> RungRefresh {
    if selected.as_ref() == current {
        RungRefresh::NoChange
    } else {
        RungRefresh::Rehome(selected)
    }
}

/// Debounce for the beacon's rung-liveness self-monitor: should
/// `consecutive_fails` failed `online()` polls count as the rung being
/// gone? `>=` [`RELAY_LIVENESS_FAILS_TO_EVICT`] so a single transient
/// blip (iroh auto-reconnects its home relay within a tick) is ignored
/// and only a sustained outage triggers a re-walk. Pure, so the
/// threshold behaviour is unit-tested without the network.
fn should_evict_rung(consecutive_fails: u32) -> bool {
    consecutive_fails >= RELAY_LIVENESS_FAILS_TO_EVICT
}

/// The next inter-round backoff for the relay-less rediscovery loop:
/// double `current`, saturating at [`RELAY_REPROBE_BACKOFF_MAX_SECS`].
/// The first wait is [`RELAY_REPROBE_BACKOFF_MIN_SECS`] (the caller
/// seeds `current` with it); thereafter the beacon keeps re-walking the
/// ladder forever, just ever more sparsely while every rung stays down.
/// Pure, so the progression is unit-tested.
fn next_relay_backoff(current: Duration) -> Duration {
    let max = Duration::from_secs(RELAY_REPROBE_BACKOFF_MAX_SECS);
    current.saturating_mul(2).min(max)
}

/// Spawn the relay-rung liveness/discovery monitor — one per co-hosted
/// beacon, run as its **own** task so relay probes never stall the
/// gossip co-host or the event loop. It **never stops probing**:
///
/// - **Homed** (`homed`, built with a rung): poll our own
///   `endpoint.online()` every [`RELAY_LIVENESS_INTERVAL_SECS`] — cheap,
///   reuses the live relay connection. `online()` resolves only when a
///   relay handshake is connected, so a `timeout` failure means the rung
///   is unreachable; [`should_evict_rung`] debounces transient blips
///   (iroh auto-reconnects within a tick). On a sustained outage, re-walk
///   the ladder once and publish the result (`Some` ⇒ re-home, `None` ⇒
///   demote to relay-less). Sticky: a healthy rung is polled forever;
///   other rungs are never probed.
/// - **Relay-less** (no rung): re-walk the ladder; a reachable rung ⇒
///   publish it; an empty round ⇒ back off ([`next_relay_backoff`]) and
///   loop.
///
/// Every publish flows to the event loop's rung-update arm, which
/// re-homes the beacon (drop + rebuild) — spawning a fresh monitor for
/// the new state. So the monitor returns after publishing a change;
/// teardown otherwise is by `beacon::Rendezvous` drop aborting it.
pub(crate) fn spawn_relay_monitor(
    endpoint: Endpoint,
    ladder: Vec<RelayUrl>,
    rung_tx: watch::Sender<Option<RelayUrl>>,
    homed: bool,
) -> JoinHandle<()> {
    let probe = Duration::from_secs(RELAY_RUNG_PROBE_SECS);
    tokio::spawn(async move {
        if homed {
            // Poll the rung we're homed on until a sustained outage.
            let mut fails: u32 = 0;
            loop {
                tokio::time::sleep(Duration::from_secs(RELAY_LIVENESS_INTERVAL_SECS)).await;
                if tokio::time::timeout(probe, endpoint.online()).await.is_ok() {
                    fails = 0;
                    continue;
                }
                fails += 1;
                if !should_evict_rung(fails) {
                    continue;
                }
                tracing::info!(
                    target: "agent_habilis_swarm::lookup",
                    fails,
                    "beacon relay rung unreachable; re-walking the ladder"
                );
                let _ = rung_tx.send(select_bootstrap_rung(&ladder, probe).await);
                return;
            }
        }
        // Relay-less: keep re-walking the ladder to rediscover a rung,
        // backing off after each fruitless round so an all-down ladder
        // isn't hammered. Never stops.
        let mut backoff = Duration::from_secs(RELAY_REPROBE_BACKOFF_MIN_SECS);
        loop {
            if let Some(rung) = select_bootstrap_rung(&ladder, probe).await {
                tracing::info!(
                    target: "agent_habilis_swarm::lookup",
                    relay = %rung,
                    "relay-less beacon rediscovered a reachable rung"
                );
                let _ = rung_tx.send(Some(rung));
                return;
            }
            tokio::time::sleep(backoff).await;
            backoff = next_relay_backoff(backoff);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        RELAY_LIVENESS_FAILS_TO_EVICT, RENDEZVOUS_RELAY_LADDER_URLS, RelayChoice, RelayUrl,
        RungRefresh, next_relay_backoff, plan_rung_refresh, relay_ladder, select_bootstrap_rung,
        select_first_reachable, should_evict_rung,
    };

    /// Tripwire: the iroh fallback rungs of our pinned rendezvous ladder
    /// must stay equal to iroh's `defaults::prod` relay hosts, **in
    /// order** (rung 1 = NA-east). Rung 0 is our own operated relay and
    /// is excluded here. Relay infra is versioned and not cross-compatible
    /// (sendme #121); if an iroh bump moves a prod relay off these hosts,
    /// the relay-direct bootstrap silently breaks for members on a
    /// different iroh version. Failing here forces a manual review (are
    /// these hosts still served?) before the bump ships.
    #[test]
    fn pinned_ladder_matches_iroh_prod() {
        use iroh::defaults::prod;
        let expected = [
            prod::NA_EAST_RELAY_HOSTNAME,
            prod::NA_WEST_RELAY_HOSTNAME,
            prod::EU_RELAY_HOSTNAME,
            prod::AP_RELAY_HOSTNAME,
        ];
        // Skip rung 0 (our own relay); the rest must track iroh's prod set.
        let ours: Vec<&str> = RENDEZVOUS_RELAY_LADDER_URLS
            .iter()
            .skip(1)
            .map(|url| {
                url.host_str()
                    .expect("ladder relay URL has a host")
                    .trim_end_matches('.')
            })
            .collect();
        let theirs: Vec<&str> = expected
            .iter()
            .map(|host| host.trim_end_matches('.'))
            .collect();
        assert_eq!(
            ours, theirs,
            "RENDEZVOUS_RELAY_LADDER drifted from iroh's prod relay set \
             (sendme #121): review docs/iroh-ecosystem-research.md before bumping iroh"
        );
    }

    #[test]
    fn pinned_ladder_resolves_to_prod_set() {
        assert_eq!(
            relay_ladder(&RelayChoice::Pinned),
            *RENDEZVOUS_RELAY_LADDER_URLS,
            "Pinned ⇒ the full n0 prod ladder"
        );
        assert!(relay_ladder(&RelayChoice::Disabled).is_empty());
        let custom = vec!["https://a.example./".parse::<RelayUrl>().unwrap()];
        assert_eq!(relay_ladder(&RelayChoice::Custom(custom.clone())), custom);
    }

    #[tokio::test]
    async fn select_bootstrap_rung_empty_ladder_is_none() {
        // Relay disabled / private ⇒ empty ladder ⇒ no probing, `None`
        // (joiners fall back to mDNS/DHT). The ordering/fall-through
        // logic is unit-tested via `select_first_reachable` below; the
        // down-detection primitive by `unreachable_rung_*` below.
        assert!(
            select_bootstrap_rung(&[], std::time::Duration::from_secs(1))
                .await
                .is_none()
        );
    }

    // --- liveness primitive (verify-first, CI-safe) -----------------
    //
    // The hardening's whole correctness rests on "online() within the
    // probe budget == this rung is genuinely connected". We can't kill a
    // real relay in CI (no iroh test-utils dep), but we CAN prove the
    // negative half without the network: an endpoint pinned to an
    // unreachable relay never reaches `online()`, so the timeout fires
    // and the rung is correctly judged down. (The positive/up→down
    // transition is the documented manual/local check.)

    #[tokio::test]
    async fn unreachable_rung_is_detected_as_down() {
        // RFC-5737 TEST-NET-1, never routable ⇒ the relay handshake can
        // never connect ⇒ `select_bootstrap_rung` finds no reachable rung.
        let bogus: RelayUrl = "https://192.0.2.1./".parse().unwrap();
        let selected = select_bootstrap_rung(&[bogus], std::time::Duration::from_secs(2)).await;
        assert!(
            selected.is_none(),
            "an unreachable relay must not be selected as a live rung"
        );
    }

    // --- liveness debounce (`should_evict_rung`) --------------------

    #[test]
    fn one_failed_poll_does_not_evict() {
        assert!(!should_evict_rung(0));
        assert!(!should_evict_rung(1), "a single blip must not re-home");
    }

    #[test]
    fn sustained_failures_evict() {
        assert!(should_evict_rung(RELAY_LIVENESS_FAILS_TO_EVICT));
        assert!(should_evict_rung(RELAY_LIVENESS_FAILS_TO_EVICT + 5));
    }

    #[test]
    fn backoff_doubles_then_saturates_at_max() {
        use std::time::Duration;
        let max = Duration::from_secs(super::RELAY_REPROBE_BACKOFF_MAX_SECS);
        let min = Duration::from_secs(crate::util::tuning::RELAY_REPROBE_BACKOFF_MIN_SECS);
        // Doubles each round...
        let mut backoff = min;
        let next = next_relay_backoff(backoff);
        assert_eq!(next, (min * 2).min(max), "first step doubles");
        // ...and walking it forward saturates at MAX, never beyond.
        for _ in 0..20 {
            backoff = next_relay_backoff(backoff);
        }
        assert_eq!(backoff, max, "backoff saturates at MAX");
        assert_eq!(next_relay_backoff(max), max, "stays capped at MAX");
    }

    // --- ladder walk (`select_first_reachable`) ---------------------
    //
    // Exhaustive, network-free coverage of "reset and start from the
    // first rung": a fake reachability predicate marks chosen rungs
    // down, and we assert which rung wins and how many were probed.

    fn rungs(hosts: &[&str]) -> Vec<RelayUrl> {
        hosts
            .iter()
            .map(|host| format!("https://{host}.example./").parse().unwrap())
            .collect()
    }

    fn short_host(url: &RelayUrl) -> String {
        url.host_str()
            .unwrap()
            .trim_end_matches('.')
            .trim_end_matches(".example")
            .to_string()
    }

    fn host_of(rung: Option<&RelayUrl>) -> Option<String> {
        rung.map(short_host)
    }

    /// Run the ladder walk with a set of hosts treated as unreachable;
    /// returns `(selected, probed_in_order)`.
    async fn walk(ladder: &[RelayUrl], down: &[&str]) -> (Option<RelayUrl>, Vec<String>) {
        let down: std::collections::HashSet<String> =
            down.iter().map(|host| (*host).to_string()).collect();
        let probed = std::cell::RefCell::new(Vec::new());
        let selected = select_first_reachable(ladder, |rung| {
            let host = short_host(&rung);
            probed.borrow_mut().push(host.clone());
            let up = !down.contains(&host);
            async move { up }
        })
        .await;
        (selected, probed.into_inner())
    }

    #[tokio::test]
    async fn ladder_picks_first_when_all_reachable() {
        let ladder = rungs(&["a", "b", "c"]);
        let (selected, probed) = walk(&ladder, &[]).await;
        assert_eq!(host_of(selected.as_ref()).as_deref(), Some("a"));
        assert_eq!(probed, vec!["a"], "stops at the first reachable rung");
    }

    #[tokio::test]
    async fn ladder_falls_through_to_next_reachable() {
        let ladder = rungs(&["a", "b", "c"]);
        // rung 0 down → rung 1 wins, having probed a then b.
        let (selected, probed) = walk(&ladder, &["a"]).await;
        assert_eq!(host_of(selected.as_ref()).as_deref(), Some("b"));
        assert_eq!(probed, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn ladder_skips_multiple_dead_prefix_rungs() {
        let ladder = rungs(&["a", "b", "c"]);
        let (selected, probed) = walk(&ladder, &["a", "b"]).await;
        assert_eq!(host_of(selected.as_ref()).as_deref(), Some("c"));
        assert_eq!(probed, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn ladder_prefers_first_rung_when_it_recovers() {
        // "reset and start from the first": the walk always re-picks the
        // earliest reachable rung, so a recovered rung 0 wins over a
        // still-up rung 1.
        let ladder = rungs(&["a", "b", "c"]);
        let (selected, _) = walk(&ladder, &[]).await;
        assert_eq!(host_of(selected.as_ref()).as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn ladder_all_down_is_none() {
        let ladder = rungs(&["a", "b", "c"]);
        let (selected, probed) = walk(&ladder, &["a", "b", "c"]).await;
        assert!(selected.is_none(), "every rung down ⇒ None (use mDNS/DHT)");
        assert_eq!(probed, vec!["a", "b", "c"], "probes the whole ladder");
    }

    #[tokio::test]
    async fn ladder_empty_is_none_without_probing() {
        let (selected, probed) = walk(&[], &[]).await;
        assert!(selected.is_none());
        assert!(probed.is_empty());
    }

    // --- steady-state failover decision (`plan_rung_refresh`) -------
    //
    // The beacon-held / relay-enabled gate lives in the event loop (it
    // owns that state); here we cover only the pure change-detection.

    fn rung(host: &str) -> RelayUrl {
        format!("https://{host}.example./").parse().unwrap()
    }

    #[test]
    fn refresh_no_op_when_current_rung_still_first_reachable() {
        let action = plan_rung_refresh(Some(rung("a")).as_ref(), Some(rung("a")));
        assert_eq!(action, RungRefresh::NoChange);
    }

    #[test]
    fn refresh_rehomes_when_current_rung_died() {
        // current rung a is gone; the walk now returns b → re-home to b.
        let action = plan_rung_refresh(Some(rung("a")).as_ref(), Some(rung("b")));
        assert_eq!(action, RungRefresh::Rehome(Some(rung("b"))));
    }

    #[test]
    fn refresh_rehomes_back_to_earlier_recovered_rung() {
        // we were on b (a had been down); a recovered → reset to a.
        let action = plan_rung_refresh(Some(rung("b")).as_ref(), Some(rung("a")));
        assert_eq!(action, RungRefresh::Rehome(Some(rung("a"))));
    }

    #[test]
    fn refresh_rehomes_to_none_when_all_rungs_die() {
        // every rung unreachable → drop the relay, lean on mDNS/DHT.
        let action = plan_rung_refresh(Some(rung("a")).as_ref(), None);
        assert_eq!(action, RungRefresh::Rehome(None));
    }

    #[test]
    fn refresh_rehomes_when_relay_recovers_from_none() {
        // we had no rung (all were down); one came back → adopt it.
        let action = plan_rung_refresh(None, Some(rung("a")));
        assert_eq!(action, RungRefresh::Rehome(Some(rung("a"))));
    }
}
