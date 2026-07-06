//! Unix-epoch timestamps — `std::time` wrappers that produce the
//! `i64`-shaped fields the wire protocol and state file expect.
//! Pure `std`, so the crate needs no `chrono` dependency.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, or 0 if the system clock is set
/// before 1970 or beyond the year 2554 (i64 fits >290 billion
/// seconds, far beyond any real system clock).
#[must_use]
pub fn unix_secs() -> i64 {
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

/// Break a Unix timestamp (seconds) into civil `(year, month, day, hour,
/// minute, second)` in UTC. Pure integer arithmetic (Howard Hinnant's
/// `civil_from_days`), so it needs no timezone / `chrono` dependency.
fn civil_from_unix(unix_secs: i64) -> (i64, i64, i64, i64, i64, i64) {
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

    (year, month, day, hour, minute, second)
}

/// Format a Unix timestamp as a compact **local**-time stamp,
/// `YYYY-MM-DD HH:MM`. Used by the `agent-square discover` picker to show when a
/// mesh was first seen, in the operator's own timezone.
///
/// `libc::localtime_r` resolves the system timezone (DST included) for
/// the given instant; if it ever fails (not reachable for a valid
/// `time_t`), we fall back to UTC via [`civil_from_unix`] so the picker
/// still renders.
#[must_use]
#[expect(
    unsafe_code,
    reason = "libc localtime_r for local-time display in the discover picker; no safe std API"
)]
pub fn local_datetime(unix_secs: i64) -> String {
    // SAFETY: a zeroed `tm` is a valid output buffer for `localtime_r`,
    // which fills the broken-down local time and returns a pointer to it
    // (or null on failure). `time` is a valid pointer to a local.
    let local = unsafe {
        let time = unix_secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&raw const time, &raw mut tm).is_null() {
            None
        } else {
            Some(tm)
        }
    };
    let (year, month, day, hour, minute) = local.map_or_else(
        || {
            let (year, month, day, hour, minute, _sec) = civil_from_unix(unix_secs);
            (year, month, day, hour, minute)
        },
        |tm| {
            (
                i64::from(tm.tm_year) + 1900,
                i64::from(tm.tm_mon) + 1,
                i64::from(tm.tm_mday),
                i64::from(tm.tm_hour),
                i64::from(tm.tm_min),
            )
        },
    );
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

#[cfg(test)]
mod tests {
    use super::{civil_from_unix, local_datetime};

    #[test]
    fn civil_from_epoch() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn civil_from_known_timestamps() {
        // The classic 1234567890 epoch — 2009-02-13T23:31:30Z.
        assert_eq!(civil_from_unix(1_234_567_890), (2009, 2, 13, 23, 31, 30));
        // A leap day with a zero clock.
        assert_eq!(civil_from_unix(1_582_934_400), (2020, 2, 29, 0, 0, 0));
    }

    #[test]
    fn local_datetime_has_expected_shape() {
        // Timezone-independent: assert the `YYYY-MM-DD HH:MM` layout
        // regardless of the host zone.
        let stamp = local_datetime(1_700_000_000);
        let bytes = stamp.as_bytes();
        assert_eq!(stamp.len(), 16, "{stamp}");
        assert_eq!(bytes[4], b'-');
        assert_eq!(bytes[7], b'-');
        assert_eq!(bytes[10], b' ');
        assert_eq!(bytes[13], b':');
        let digits = stamp.chars().filter(char::is_ascii_digit).count();
        assert_eq!(digits, 12, "{stamp}");
    }
}
