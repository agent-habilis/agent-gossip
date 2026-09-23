use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::time::Duration;

use crate::runtime_base;

const ACQUIRE_RETRY_FOR: Duration = Duration::from_millis(500);
const ACQUIRE_RETRY_EVERY: Duration = Duration::from_millis(10);

fn lock_path(mesh: &str, nickname: &str) -> PathBuf {
    fofoca::util::mesh_runtime_dir(&runtime_base(), mesh).join(format!("{nickname}.bell"))
}

/// Hold the bell lock for as long as the returned file lives. `None` when a
/// duplicate bell already holds it, or the mesh dir is gone (no daemon).
///
/// An OS lock, not daemon state: the kernel drops it the instant the bell
/// dies, while a daemon-side waiter carries no identity, empties between the
/// CLI's re-issues, and outlives a killed bell until its deadline. The file
/// is never unlinked: removing it while another bell holds it would split the
/// lock across two inodes.
pub(crate) async fn acquire(mesh: &str, nickname: &str) -> Option<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path(mesh, nickname))
        .ok()?;
    // Retry to ride out a probe's momentary shared lock.
    let deadline = tokio::time::Instant::now() + ACQUIRE_RETRY_FOR;
    loop {
        match file.try_lock() {
            Ok(()) => return Some(file),
            Err(TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(ACQUIRE_RETRY_EVERY).await;
            }
            Err(_) => return None,
        }
    }
}

/// A shared probe, so parallel probes (duplicate hooks, `session`) never see
/// each other as a bell.
pub(crate) fn is_armed(mesh: &str, nickname: &str) -> bool {
    let Ok(file) = File::open(lock_path(mesh, nickname)) else {
        return false;
    };
    matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock))
}
