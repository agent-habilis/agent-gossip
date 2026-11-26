use anyhow::Result;
use habilis_network::runtime::state_file::SessionIdentity;
use habilis_network::runtime::{state_file, tuning};

use super::args::ReadyOpts;
use super::print_ready_identity::print_ready_identity;
use super::read_member::read_member;
use super::start_failure;

/// A state file that `wait_for_ready` saw report a fresh `ready: true`. Only
/// `wait_for_ready` builds one, so an identity read through it follows a passed
/// readiness check.
pub(super) struct ReadyStateFile {
    path: std::path::PathBuf,
}

impl ReadyStateFile {
    pub(super) fn identity(&self) -> SessionIdentity {
        state_file::read_identity(&self.path)
    }

    pub(super) fn member(&self) -> serde_json::Value {
        read_member(&self.path)
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
    let began = start_failure::now_secs();
    let deadline = now
        .checked_add(std::time::Duration::from_secs(timeout_secs))
        .unwrap_or_else(|| now + tuning::READY_MAX);
    loop {
        // Read off the runtime's blocking pool: a `--state-file` on a hung
        // mount must not block a worker thread. A read error (malformed/torn/
        // old-schema file) is treated as not-ready, not a hard failure — the
        // daemon may be mid-overwrite. A *persistent* error (e.g. a bad path)
        // can't self-heal and just spins to the deadline, so log it so the
        // cause is recoverable.
        let path = state_file.to_path_buf();
        let read = tokio::task::spawn_blocking(move || state_file::read_snapshot(&path)).await?;
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
        // A start that failed will never serve: say why now, not at the deadline. A daemon that is
        // killed or panics leaves no such error, and the wait runs to its limit as before.
        if let Some(reason) = start_failure::read(state_file, began) {
            anyhow::bail!(
                "the daemon at {} ended before it was ready: {reason}",
                state_file.display()
            );
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "daemon at {} not ready within {timeout_secs}s",
                state_file.display()
            );
        }
        tokio::time::sleep(tuning::READY_POLL_INTERVAL).await;
    }
}

/// True when a `ready: true` state-file write is recent enough to trust —
/// `last_updated` within `READY_FRESH` of now in *either* direction. A
/// live daemon refreshes `last_updated` every `STATE_REFRESH_SECS`, so a value
/// older than the window means the writer is gone (e.g. a leftover file from a
/// daemon killed by SIGKILL). A value far in the *future* is equally
/// untrustworthy — a stale file whose age only looks recent because the wall
/// clock stepped backward (NTP correction, VM restore) — so it is rejected
/// too. `UnixSecs::now` is non-negative, so the `i64` math below cannot
/// underflow into the past.
pub(super) fn ready_is_fresh(last_updated: u64) -> bool {
    let now = habilis_network::protocol::UnixSecs::now().as_secs();
    let last_updated = i64::try_from(last_updated).unwrap_or(i64::MAX);
    let skew = now - last_updated; // >0: file is in the past; <0: in the future
    let window = i64::try_from(tuning::READY_FRESH.as_secs()).unwrap_or(i64::MAX);
    skew.abs() <= window
}

/// Block until the daemon's `--state-file` reports it is *freshly* serving
/// (the `ready` flag is `true` and `last_updated` is recent), then exit 0.
/// Prints `{mesh,name,nickname}` on success, so the gate doubles as the
/// identity read instead of the caller parsing the file. Times out non-zero.
///
/// Robustness: a missing, unreadable, malformed, or stale-but-not-yet-ready
/// file is "not ready yet, keep polling" — only the deadline fails the gate.
/// This tolerates a half-written or old-schema file the daemon is about to
/// atomically overwrite, and the freshness check rejects a `ready: true`
/// left behind by a prior daemon that was killed with SIGKILL.
pub(super) async fn ready(options: ReadyOpts) -> Result<()> {
    let ReadyOpts {
        state_file,
        timeout_secs,
        legacy_output: _,
    } = options;
    let ready = wait_for_ready(&state_file, timeout_secs).await?;
    print_ready_identity(&ready);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::cli::start_failure;

    #[tokio::test]
    async fn a_start_that_failed_ends_the_wait_at_once_with_its_error() {
        let dir = std::env::temp_dir().join(format!("ready-failed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        let state_file = dir.join("session.json");
        let text = "max connections is 4: the lowest value is 8 (the default is 16)";
        start_failure::record(&state_file, &anyhow::anyhow!(text));

        let started = Instant::now();
        let outcome = super::wait_for_ready(&state_file, 5).await;
        let waited = started.elapsed();

        let error = outcome
            .err()
            .expect("a daemon that never served is not ready");
        assert!(
            waited < Duration::from_secs(2),
            "ready must not wait its 5 s limit for a daemon that already ended: waited {waited:?}"
        );
        assert!(
            error.to_string().contains(text),
            "the error of the failed start reaches the caller: {error}"
        );
    }
}
