//! This process's **resident memory** — the physical RAM it currently holds
//! (a.k.a. the resident set size, RSS) — for the warn-only leak signal.
//!
//! The distributed soak that crashed a host had **no in-process leak
//! visibility** — resident memory was only observable from an external `ps`
//! sampler. This reads our own so the daemon can emit a `warn` when it crosses
//! a soft threshold ([`crate::util::tuning::resident_memory_warn_mb`]).
//! Warn-only by design: host safety comes from the a2a runbook's OS resource
//! caps, not from the daemon exiting.

/// This process's peak resident memory in **bytes**, or `None` if it can't
/// be read on this platform.
///
/// Backed by `getrusage(RUSAGE_SELF).ru_maxrss`. We report *peak* rather than
/// instantaneous deliberately: a memory leak climbs monotonically, so peak ==
/// current in the case we care about, and crossing the threshold is exactly
/// the leak signal — with one cheap syscall and no `/proc` parsing or mach FFI.
#[must_use]
pub(crate) fn peak_resident_memory_bytes() -> Option<u64> {
    maxrss_bytes()
}

/// This process's peak resident memory in `MiB`, or `None` on platforms where
/// it can't be read.
///
/// The leak gauge the periodic `mesh census` log line carries: peak resident
/// memory is monotonic, so a churn-proportional leak shows as a rising slope on
/// one always-on timeline next to the roster/link counts — no external `ps`
/// sampler needed (the gap the distributed-soak runbook had to work around).
#[must_use]
pub(crate) fn peak_resident_memory_mb() -> Option<u64> {
    peak_resident_memory_bytes().map(|bytes| bytes / (1024 * 1024))
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
    use super::peak_resident_memory_bytes;

    #[cfg(unix)]
    #[test]
    fn reads_a_plausible_resident_memory() {
        // The test process holds a real address space, so resident memory is
        // readable and non-trivial — guards against a units/selector regression
        // returning 0.
        let bytes = peak_resident_memory_bytes().expect("resident memory readable on unix");
        assert!(
            bytes > 1024 * 1024,
            "resident memory should exceed 1 MiB, got {bytes} bytes"
        );
    }
}
