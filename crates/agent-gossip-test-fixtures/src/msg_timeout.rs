use std::time::Duration;

/// Steady-state delivery budget: how long a meshed peer may take to surface a
/// message/presence/task leg. The suite-wide standard for every positive
/// (adaptive, break-on-success) delivery wait. A meshed in-process round trip
/// is normally sub-second; the headroom is for a loaded CI host where two
/// concurrent in-process meshes can stall a delivery well past a tighter
/// budget (crypto-heavy deps are release-optimized even in dev builds via the
/// `[profile.dev.package]` overrides, so debug crypto is no longer the
/// bottleneck). `wait_for`/`wait_until` are adaptive, so a healthy run returns
/// immediately and only a genuine stall pays the ceiling.
pub const MSG_TIMEOUT: Duration = Duration::from_mins(1);
