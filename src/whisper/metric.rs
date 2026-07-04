//! Per-link routing cost — an ETX/ETT-style composite where **lower is better**
//! and costs **add across hops**. Kept an integer so it is `Ord` for Dijkstra.
//!
//! The research is decisive that hop-count is the wrong metric (it favours the
//! longest, weakest links); a delivery-ratio/RTT composite is the settled mesh
//! result. This type is the composite's carrier; how a neighbour's live
//! telemetry maps into it lives with the telemetry layer.

/// The cost of one link, or an accumulated path cost. Lower is better;
/// [`LinkMetric::INFINITE`] marks an unusable link, and any path crossing one
/// is itself unusable. Addition **saturates** so an unusable path can never wrap
/// around to a cheap one.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub(crate) struct LinkMetric(pub u32);

impl LinkMetric {
    /// A zero-cost link — the identity for path accumulation (cost of staying
    /// put at the source).
    pub(crate) const ZERO: Self = Self(0);

    /// An unusable link: never selected, and any path crossing it is unusable.
    pub(crate) const INFINITE: Self = Self(u32::MAX);

    /// Accumulate this link's cost onto the rest of a path (saturating), so a
    /// path through an [`INFINITE`](Self::INFINITE) link stays unusable.
    pub(crate) fn add(self, rest: Self) -> Self {
        Self(self.0.saturating_add(rest.0))
    }

    /// Whether this link can carry traffic at all.
    pub(crate) fn is_usable(self) -> bool {
        self != Self::INFINITE
    }
}
