//! This process's resident-set size (RSS), for the warn-only leak signal.
//!
//! The distributed soak that crashed a host had **no in-process leak
//! visibility** — RSS was only observable from an external `ps` sampler. This
//! reads our own RSS so the daemon can emit a `warn` when it crosses a soft
//! threshold (`AHS_RSS_WARN_MB`, see [`crate::util::tuning::rss_warn_mb`]).
//! Warn-only by design: host safety comes from the e2e runbook's OS resource
//! caps, not from the daemon exiting.

/// This process's peak resident-set size in **bytes**, or `None` if it can't
/// be read on this platform.
///
/// Backed by `getrusage(RUSAGE_SELF).ru_maxrss`. We report *peak* rather than
/// instantaneous RSS deliberately: a memory leak climbs monotonically, so peak
/// == current in the case we care about, and crossing the threshold is exactly
/// the leak signal — with one cheap syscall and no `/proc` parsing or mach FFI.
#[must_use]
pub(crate) fn peak_rss_bytes() -> Option<u64> {
    maxrss_bytes()
}

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "libc getrusage FFI; no safe wrapper for RUSAGE_SELF maxrss"
)]
fn maxrss_bytes() -> Option<u64> {
    // SAFETY: an all-zero `rusage` is a valid input value; `getrusage` fully
    // overwrites the fields it reports. We pass a valid unique pointer to it
    // and the documented `RUSAGE_SELF` selector.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) != 0 {
            return None;
        }
        usage
    };
    let maxrss = u64::try_from(usage.ru_maxrss).ok()?;
    // `ru_maxrss` units differ by platform: KiB on Linux, bytes on macOS/BSD.
    #[cfg(target_os = "linux")]
    {
        Some(maxrss.saturating_mul(1024))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Some(maxrss)
    }
}

#[cfg(not(unix))]
fn maxrss_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::peak_rss_bytes;

    #[cfg(unix)]
    #[test]
    fn reads_a_plausible_rss() {
        // The test process holds a real address space, so RSS is readable and
        // non-trivial — guards against a units/selector regression returning 0.
        let rss = peak_rss_bytes().expect("RSS readable on unix");
        assert!(rss > 1024 * 1024, "RSS should exceed 1 MiB, got {rss} bytes");
    }
}
