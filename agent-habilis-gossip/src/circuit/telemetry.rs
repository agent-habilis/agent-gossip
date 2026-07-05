//! Per-neighbour **local** telemetry: a lightweight profile of each directly
//! connected peer, measured by us and never trusted from the peer's own claims
//! (the I2P rule). Its output is a [`LinkMetric`] for *our own* links, which we
//! advertise in link-state gossip — standard link-state trust: a node reports
//! only the links it measures itself.
//!
//! The composite follows the ETX/ETT idea in integer fixed-point: cost rises as
//! smoothed RTT grows and as the delivery ratio drops. A link that only ever
//! fails is [`LinkMetric::INFINITE`].

use std::time::Duration;

use super::metric::LinkMetric;

/// Assumed RTT (ms) for a neighbour we haven't timed yet, so a fresh link has a
/// usable (not free, not infinite) cost.
const DEFAULT_RTT_MS: u32 = 50;

/// A neighbour is "reliable" (fast tier, preferred for hop selection) when its
/// composite metric is at or below this — roughly a healthy sub-`DEFAULT` link.
const RELIABLE_METRIC_MAX: u32 = 60;

/// A local, passively-measured profile of one directly connected neighbour.
#[derive(Debug, Clone, Default)]
pub struct NeighborProfile {
    /// Smoothed RTT in milliseconds, or `None` until the first sample.
    srtt_ms: Option<u32>,
    /// Observed circuit/probe successes and failures (delivery-ratio inputs).
    successes: u32,
    failures: u32,
}

impl NeighborProfile {
    /// Fold an RTT sample into the smoothed estimate (EWMA).
    pub(crate) fn record_rtt(&mut self, sample: Duration) {
        let sample_ms = u32::try_from(sample.as_millis()).unwrap_or(u32::MAX);
        self.srtt_ms = Some(match self.srtt_ms {
            None => sample_ms,
            // EWMA with α = 1/8 (classic TCP smoothing): (sample + 7·old) / 8,
            // in u64 so 7·old can't overflow.
            Some(old) => {
                let blended = (u64::from(sample_ms) + u64::from(old) * 7) / 8;
                u32::try_from(blended).unwrap_or(u32::MAX)
            }
        });
    }

    /// Record a delivered/failed circuit or probe over this link.
    pub(crate) fn record_success(&mut self) {
        self.successes = self.successes.saturating_add(1);
    }

    pub(crate) fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    /// The composite link cost we advertise for this neighbour. Rises with RTT
    /// and with lost deliveries; a link observed to *only* fail is unusable.
    pub(crate) fn metric(&self) -> LinkMetric {
        let total = self.successes.saturating_add(self.failures);
        if total > 0 && self.successes == 0 {
            return LinkMetric::INFINITE; // only failures seen — don't route over it
        }
        let rtt_ms = u64::from(self.srtt_ms.unwrap_or(DEFAULT_RTT_MS));
        // ETX in thousandths: 1/delivery = total/successes. No data yet ⇒ assume
        // perfect delivery (1000) so a fresh link is judged on RTT alone.
        let etx_milli = if self.successes == 0 {
            1000
        } else {
            u64::from(total) * 1000 / u64::from(self.successes)
        };
        let cost = rtt_ms * etx_milli / 1000;
        // Never silently collapse a finite cost onto INFINITE.
        LinkMetric(u32::try_from(cost).unwrap_or(u32::MAX - 1))
    }

    /// Whether this neighbour sits in the fast/reliable tier preferred for hop
    /// selection.
    pub(crate) fn is_reliable(&self) -> bool {
        let metric = self.metric();
        metric.is_usable() && metric.0 <= RELIABLE_METRIC_MAX
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{DEFAULT_RTT_MS, LinkMetric, NeighborProfile};

    #[test]
    fn fresh_profile_uses_default_rtt_and_perfect_delivery() {
        let profile = NeighborProfile::default();
        assert_eq!(profile.metric(), LinkMetric(DEFAULT_RTT_MS));
    }

    #[test]
    fn srtt_smooths_toward_samples() {
        let mut profile = NeighborProfile::default();
        profile.record_rtt(Duration::from_millis(80));
        // First sample seeds srtt exactly.
        assert_eq!(profile.metric(), LinkMetric(80));
        // A lower sample pulls it down but not all the way (EWMA).
        profile.record_rtt(Duration::from_millis(8));
        let smoothed = profile.metric().0;
        assert!(
            smoothed < 80 && smoothed > 8,
            "smoothed between old and new, got {smoothed}"
        );
    }

    #[test]
    fn lost_deliveries_raise_the_cost() {
        let mut good = NeighborProfile::default();
        good.record_rtt(Duration::from_millis(20));
        for _ in 0..10 {
            good.record_success();
        }
        let mut lossy = NeighborProfile::default();
        lossy.record_rtt(Duration::from_millis(20));
        for _ in 0..5 {
            lossy.record_success();
            lossy.record_failure(); // 50% delivery ⇒ ETX 2 ⇒ ~2× the cost
        }
        assert!(
            lossy.metric() > good.metric(),
            "a lossy link must cost more than a clean one at equal RTT"
        );
    }

    #[test]
    fn only_failures_is_unusable() {
        let mut dead = NeighborProfile::default();
        dead.record_failure();
        dead.record_failure();
        assert_eq!(dead.metric(), LinkMetric::INFINITE);
        assert!(!dead.is_reliable());
    }

    #[test]
    fn fast_clean_link_is_reliable() {
        let mut profile = NeighborProfile::default();
        profile.record_rtt(Duration::from_millis(15));
        profile.record_success();
        assert!(profile.is_reliable());
    }
}
