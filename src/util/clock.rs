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

/// Format a Unix timestamp (seconds) as ISO-8601 UTC —
/// `YYYY-MM-DDTHH:MM:SSZ`. Pure integer arithmetic (Howard Hinnant's
/// `civil_from_days`), so it needs no timezone / `chrono` dependency.
/// Used by the `ahs discover` picker to show when a swarm was first seen.
#[must_use]
pub(crate) fn iso8601_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;

    // civil-from-days: days since 1970-01-01 → (year, month, day).
    // Shift the epoch to 0000-03-01 so leap days fall at the end of the
    // 400-year era and the arithmetic stays branch-free.
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year_base = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_phase = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = day_of_year - (153 * month_phase + 2) / 5 + 1; // [1, 31]
    let month = if month_phase < 10 {
        month_phase + 3
    } else {
        month_phase - 9
    }; // [1, 12]
    let year = if month <= 2 { year_base + 1 } else { year_base };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::iso8601_utc;

    #[test]
    fn formats_epoch() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn formats_known_timestamps() {
        // 2009-02-13T23:31:30Z — the classic 1234567890 epoch.
        assert_eq!(iso8601_utc(1_234_567_890), "2009-02-13T23:31:30Z");
        // A leap-year date with a non-zero clock.
        assert_eq!(iso8601_utc(1_582_934_400), "2020-02-29T00:00:00Z");
    }
}
