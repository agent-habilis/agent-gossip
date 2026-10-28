use std::time::Duration;

/// Delivery budget for a **big (unlogged) multi-shard body**.
///
/// A large shard group skips the message log by design — it must not evict the
/// anti-entropy history — so anti-entropy cannot heal it. A dropped shard is
/// recovered only by the receiver's `shard/repair` ask, and that path is
/// deliberately lazy: the partial group has to sit idle for
/// `REASSEMBLY_REPAIR_IDLE_SECS` (60s) *and* the check runs on a one-minute
/// prune tick, so a repair round can land anywhere up to ~2 minutes out.
///
/// Asserting a big body's arrival within the steady-state `MSG_TIMEOUT` (1 min)
/// therefore asserts that **no shard is ever dropped** — which the design does
/// not promise; the repair path exists precisely because one can be. A budget
/// that cannot clear a full repair cycle turns a single lost shard into a test
/// failure, which is what made the multi-shard tests flaky. Adaptive like every
/// other wait here: a healthy run returns in well under a second, and only a
/// genuine stall pays the ceiling.
pub const BIG_BODY_TIMEOUT: Duration = Duration::from_mins(3);
