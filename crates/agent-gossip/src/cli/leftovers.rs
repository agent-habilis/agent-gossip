use std::fs::File;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use habilis_network::runtime::{DaemonState, read_record, registry};
use habilis_network::util::mesh_runtime_dir;

/// The tracing log of a member that does not run is the record of why its run ended. It stays this long
/// before the scan removes it.
const DEAD_LOG_KEEP: Duration = Duration::from_hours(7 * 24);

/// Remove the tracing logs in the runtime base that are old and have no member behind them, and the mesh
/// folder that this removal emptied. It takes no lock and asks no daemon: it reads only the age of a file
/// and whether a socket sits beside it.
///
/// A daemon removes its own socket and chunk folder at a clean stop. What a killed daemon leaves is
/// removed by [`sweep_dead_members`].
pub(super) fn sweep_logs(base: &Path) {
    sweep_orphan_logs(base, SystemTime::now());
}

/// Remove the control socket and the chunk folder of each member whose daemon was killed. A member is
/// dead when its lock file holds a record that names a process that is gone, or a process with another
/// start (the pid was reused), and nobody holds its lock.
///
/// Nothing waits, and no daemon is asked. The lock is tried without blocking and kept for the delete: a
/// daemon that starts for the member takes the lock first, so it never loses the socket that it just
/// made, and a start that comes while the lock is kept is refused as it is in the stop wait of `leave`.
pub(super) fn sweep_dead_members(base: &Path) {
    let Ok(locks) = registry(base) else {
        return;
    };
    for lock in locks.flatten() {
        sweep_dead_member(base, &lock);
    }
}

fn sweep_dead_member(base: &Path, lock: &Path) {
    let dead =
        |record: &habilis_network::runtime::DaemonRecord| record.state() == DaemonState::Gone;
    let Some(record) = read_record(lock).ok().flatten().filter(dead) else {
        return;
    };
    let Ok(file) = File::open(lock) else {
        return;
    };
    // A running daemon holds this lock, and so does a daemon that is starting. Either way nothing is
    // removed. Any other failure of the try also leaves the files.
    if file.try_lock().is_err() {
        return;
    }
    // The record is read again with the lock kept: a daemon that ended its start before the try wrote a
    // new record, and that record is not the one that was judged.
    if read_record(lock).ok().flatten().as_ref() != Some(&record) {
        return;
    }
    let dir = mesh_runtime_dir(base, record.mesh.as_str());
    remove_socket(&dir.join(format!("{}.ipc.sock", record.nickname.as_str())));
    remove_folder(&dir.join(format!("{}.chunks", record.nickname.as_str())));
}

fn remove_socket(path: &Path) {
    // `symlink_metadata` does not follow a link, so a link in this place is never removed.
    let is_socket = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_socket());
    if is_socket {
        let _ = std::fs::remove_file(path);
    }
}

fn remove_folder(path: &Path) {
    let is_folder = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_dir());
    if is_folder {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// A log with no socket beside it that is 7 days old. A member that runs has its socket, so its log is
/// skipped.
fn sweep_orphan_logs(base: &Path, now: SystemTime) {
    let member_folders = std::fs::read_dir(base).into_iter().flatten().flatten();
    for folder in member_folders {
        let dir = folder.path();
        let is_folder = std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_dir());
        if !is_folder {
            continue;
        }
        let mut removed_any = false;
        for file in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = file.file_name();
            let Some(nickname) = name
                .to_str()
                .and_then(|name| name.strip_suffix(".tracing.log"))
            else {
                continue;
            };
            let socket = dir.join(format!("{nickname}.ipc.sock"));
            if std::fs::symlink_metadata(socket).is_err() && remove_old_log(&file.path(), now) {
                removed_any = true;
            }
        }
        // `remove_dir` refuses a folder that holds anything. Only a folder that this scan emptied goes: an
        // empty folder that the scan did not empty can be the folder that a starting daemon just made.
        if removed_any {
            let _ = std::fs::remove_dir(&dir);
        }
    }
}

fn remove_old_log(path: &Path, now: SystemTime) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.file_type().is_file() {
        return false;
    }
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    // A file dated in the future gives no age, and it is not old.
    let Ok(age) = now.duration_since(modified) else {
        return false;
    };
    age >= DEAD_LOG_KEEP && std::fs::remove_file(path).is_ok()
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use super::{DEAD_LOG_KEEP, sweep_orphan_logs};

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("leftovers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("the temp dir is writable");
        base
    }

    /// A socket file that no process listens at: what a `SIGKILL`ed daemon leaves.
    fn stale_socket(path: &Path) {
        drop(UnixListener::bind(path).expect("the folder is writable"));
    }

    #[test]
    fn an_old_log_with_no_socket_goes_and_one_beside_a_socket_stays() {
        let base = scratch("orphan");
        let gone = base.join("goneone");
        let running = base.join("runnings");
        std::fs::create_dir_all(&gone).expect("writable");
        std::fs::create_dir_all(&running).expect("writable");
        let orphan = gone.join("dead-one.tracing.log");
        std::fs::write(&orphan, "log").expect("writable");
        let kept = running.join("live-two.tracing.log");
        std::fs::write(&kept, "log").expect("writable");
        stale_socket(&running.join("live-two.ipc.sock"));
        let much_later = SystemTime::now() + DEAD_LOG_KEEP * 2;

        sweep_orphan_logs(&base, SystemTime::now());
        assert!(
            orphan.exists(),
            "a log that is not old is the record of a recent run"
        );

        sweep_orphan_logs(&base, much_later);
        assert!(
            !orphan.exists(),
            "a week-old log of a member that is not running is only disk use"
        );
        assert!(!gone.exists(), "and its empty folder with it");
        assert!(
            kept.exists(),
            "a log beside a socket is a member that this scan does not judge"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_empty_folder_that_the_scan_did_not_empty_stays() {
        let base = scratch("fresh-folder");
        let fresh = base.join("freshfolder");
        std::fs::create_dir_all(&fresh).expect("writable");

        sweep_orphan_logs(&base, SystemTime::now() + DEAD_LOG_KEEP * 2);

        assert!(
            fresh.exists(),
            "a folder that the scan did not empty can be the folder of a daemon that is starting"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_daemon_locks_folder_of_the_base_is_never_swept() {
        let base = scratch("locks");
        let dir = base.join("meshfolder");
        let locks = base.join("locks");
        std::fs::create_dir_all(&dir).expect("writable");
        std::fs::create_dir_all(&locks).expect("writable");
        let lock = locks.join("meshfolder-one-two.lock");
        std::fs::write(&lock, "4242").expect("writable");
        std::fs::write(dir.join("one-two.tracing.log"), "log").expect("writable");

        sweep_orphan_logs(&base, SystemTime::now() + DEAD_LOG_KEEP * 10);

        assert!(!dir.exists(), "the folder that the scan emptied goes");
        assert!(
            lock.exists(),
            "a lock file is never removed, so that two starts keep one lock"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
