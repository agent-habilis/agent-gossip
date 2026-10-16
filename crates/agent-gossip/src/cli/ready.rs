use anyhow::Result;

/// A state file that `wait_for_ready` saw report a fresh `ready: true`. Only
/// `wait_for_ready` builds one, so an identity read through it follows a passed
/// readiness check.
pub(super) struct ReadyStateFile {
    path: std::path::PathBuf,
}

impl ReadyStateFile {
    pub(super) fn identity(&self) -> habilis_network::runtime::state_file::SessionIdentity {
        habilis_network::runtime::state_file::read_identity(&self.path)
    }
}

/// Block until `state_file` reports a fresh `ready: true`, or fail at the
/// deadline. The wait behind both the `ready` gate and `poll --state-file`.
pub(super) async fn wait_for_ready(
    state_file: &std::path::Path,
    timeout_secs: u64,
) -> Result<ReadyStateFile> {
    // Saturating add: an absurd `--timeout-secs` must not panic the gate
    // (`Instant + Duration` panics on overflow); clamp to a far-future
    // deadline instead.
    let now = tokio::time::Instant::now();
    let deadline = now
        .checked_add(std::time::Duration::from_secs(timeout_secs))
        .unwrap_or_else(|| {
            now + std::time::Duration::from_secs(habilis_network::runtime::tuning::READY_MAX_SECS)
        });
    loop {
        // Read off the runtime's blocking pool: a `--state-file` on a hung
        // mount must not block a worker thread. A read error (malformed/torn/
        // old-schema file) is treated as not-ready, not a hard failure — the
        // daemon may be mid-overwrite. A *persistent* error (e.g. a bad path)
        // can't self-heal and just spins to the deadline, so log it so the
        // cause is recoverable.
        let path = state_file.to_path_buf();
        let read = tokio::task::spawn_blocking(move || {
            habilis_network::runtime::state_file::read_snapshot(&path)
        })
        .await?;
        match read {
            Ok(Some(snapshot)) if snapshot.ready && ready_is_fresh(snapshot.last_updated) => {
                return Ok(ReadyStateFile {
                    path: state_file.to_path_buf(),
                });
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(%error, path = %state_file.display(), "agent-gossip ready: state-file read failed; retrying");
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "daemon at {} not ready within {timeout_secs}s",
                state_file.display()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(
            habilis_network::runtime::tuning::READY_POLL_INTERVAL_MS,
        ))
        .await;
    }
}

/// True when a `ready: true` state-file write is recent enough to trust —
/// `last_updated` within `READY_FRESH_SECS` of now in *either* direction. A
/// live daemon refreshes `last_updated` every `STATE_REFRESH_SECS`, so a value
/// older than the window means the writer is gone (e.g. a leftover file from a
/// daemon killed by SIGKILL). A value far in the *future* is equally
/// untrustworthy — a stale file whose age only looks recent because the wall
/// clock stepped backward (NTP correction, VM restore) — so it is rejected
/// too. `clock::unix_secs` is non-negative, so the `i64` math below cannot
/// underflow into the past.
pub(super) fn ready_is_fresh(last_updated: u64) -> bool {
    let now = habilis_network::util::clock::unix_secs();
    let last_updated = i64::try_from(last_updated).unwrap_or(i64::MAX);
    let skew = now - last_updated; // >0: file is in the past; <0: in the future
    let window =
        i64::try_from(habilis_network::runtime::tuning::READY_FRESH_SECS).unwrap_or(i64::MAX);
    skew.abs() <= window
}
