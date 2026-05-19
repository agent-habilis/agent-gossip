//! Unix-epoch timestamps — `std::time` wrappers that produce the
//! `i64`-shaped fields the wire protocol and state file expect.
//!
//! Replaces the `chrono::Utc::now().timestamp()` shape that used to
//! pull `chrono` (and its `iana-time-zone`, `num-traits` etc.) for
//! three call sites.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, or 0 if the system clock is set
/// before 1970 or beyond the year 2554 (i64 fits >290 billion
/// seconds, far beyond any real system clock).
pub(crate) fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| i64::try_from(since_epoch.as_secs()).ok())
        .unwrap_or(0)
}

/// Nanoseconds since the Unix epoch. Returns 0 on pre-1970 clocks
/// or after the year 2262 (i64 nanosecond overflow). Only used by
/// tests today (unique tmp-file suffixes); gated on `#[cfg(test)]`
/// to avoid a dead-code lint in release builds.
#[cfg(test)]
pub(crate) fn unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| i64::try_from(since_epoch.as_nanos()).ok())
        .unwrap_or(0)
}
