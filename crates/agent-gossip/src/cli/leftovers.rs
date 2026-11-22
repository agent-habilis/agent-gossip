use std::io::ErrorKind;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tokio::net::UnixStream;

use crate::runtime_base;

/// The tracing log of a dead member is the record of why its run ended. It stays this long before
/// the sweep removes it.
const DEAD_LOG_KEEP: Duration = Duration::from_hours(7 * 24);

/// A connection that does not come back in this time proves nothing, so it does not count as refused.
const CONNECT_LIMIT: Duration = Duration::from_secs(2);

/// Whether nothing listens at `socket`: the connection is refused, or the file is gone. A stopped or
/// slow daemon still accepts the connection or answers late, and it is not refused.
pub(super) async fn is_refused(socket: &Path) -> bool {
    let attempt = tokio::time::timeout(CONNECT_LIMIT, UnixStream::connect(socket)).await;
    matches!(
        attempt,
        Ok(Err(error)) if matches!(error.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound)
    )
}

/// Remove what the members behind these sockets left in the runtime base. The caller passes the
/// sockets that refused a connection. Each one is asked again just before the sweep: a member that
/// restarted in between answers now, and its files stay.
///
/// Two races remain, each a window of microseconds. A member with the same mesh and nickname that
/// restarts between the second check and the removal loses its socket and its chunk folder until it
/// restarts again. The member key lock would close that window, but it sits beside the state file,
/// and the state file of a dead member is unknown when it is outside the runtime base. A member that
/// restarts after 7 days appends to a log that is 7 days old, and the orphan scan can remove that log
/// before the first write, so one log is lost.
pub(super) async fn sweep(dead_sockets: &[PathBuf]) {
    let base = runtime_base();
    for socket in dead_sockets {
        if is_refused(socket).await {
            sweep_member(&base, socket, SystemTime::now());
        }
    }
    sweep_orphan_logs(&base, SystemTime::now());
}

/// A sweep removes the socket of a dead member with its chunk folder, and nothing then names the
/// member any more. Its tracing log is the one file left, so this scan finds it: a log with no
/// socket beside it that is 7 days old. A member that runs has its socket, so its log is skipped.
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
        // `remove_dir` refuses a folder that holds anything. Only a folder that this scan emptied goes.
        if removed_any {
            let _ = std::fs::remove_dir(&dir);
        }
    }
}

/// The folder and the nickname of the member that a socket path names, or `None` when the path is
/// not `<base>/<mesh folder>/<nickname>.ipc.sock`.
fn member_of<'path>(base: &Path, socket: &'path Path) -> Option<(&'path Path, &'path str)> {
    let dir = socket.parent().filter(|dir| dir.parent() == Some(base))?;
    let file_name = socket.file_name()?.to_str()?;
    let nickname = file_name
        .strip_suffix(".ipc.sock")
        .filter(|nickname| !nickname.is_empty())?;
    Some((dir, nickname))
}

/// A dead member leaves its socket, its chunk folder (a cache that its next run would clear), and
/// its tracing log. The socket and the chunk folder go at once, the log when it is old, and the
/// member folder when nothing else is in it.
fn sweep_member(base: &Path, socket: &Path, now: SystemTime) {
    let Some((dir, nickname)) = member_of(base, socket) else {
        return;
    };
    // \`active_socket_paths\` follows a link, so a socket can be listed through a mesh folder that is a link
    // to a place outside the base. A path inside such a folder is checked through the link, and the sweep
    // would remove files that are not the daemon's.
    let is_real_folder =
        std::fs::symlink_metadata(dir).is_ok_and(|metadata| metadata.file_type().is_dir());
    if !is_real_folder {
        return;
    }
    let removed = [
        remove_socket(socket),
        remove_folder(&dir.join(format!("{nickname}.chunks"))),
        remove_old_log(&dir.join(format!("{nickname}.tracing.log")), now),
    ];
    // `remove_dir` refuses a folder that holds anything, so the folder of a live member, or one with
    // a file of another kind, stays.
    if removed.contains(&true) {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Each of the three `remove_` functions says whether it removed something. A caller removes the
/// folder only after something in it went: an empty folder that the sweep did not empty can be the
/// folder that a starting daemon just made, and the daemon has not yet written its first file in it.
fn remove_socket(path: &Path) -> bool {
    let is_socket =
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket());
    is_socket && std::fs::remove_file(path).is_ok()
}

fn remove_folder(path: &Path) -> bool {
    // `symlink_metadata` does not follow a link, so a link in this place is never walked into.
    let is_folder =
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir());
    is_folder && std::fs::remove_dir_all(path).is_ok()
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
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use super::{DEAD_LOG_KEEP, is_refused, member_of, sweep_member, sweep_orphan_logs};

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
    fn a_socket_path_names_its_member_only_inside_a_mesh_folder_of_the_base() {
        let base = Path::new("/run/base");
        let socket = base.join("meshfolder").join("one-two.ipc.sock");
        assert_eq!(
            member_of(base, &socket),
            Some((base.join("meshfolder").as_path(), "one-two")),
            "a socket of a member names its folder and its nickname"
        );
        let elsewhere = Path::new("/home/user/one-two.ipc.sock");
        assert!(
            member_of(base, elsewhere).is_none(),
            "a socket outside the base is never swept"
        );
        let in_base = base.join("one-two.ipc.sock");
        assert!(
            member_of(base, &in_base).is_none(),
            "a file that sits in the base itself names no member folder"
        );
        let no_nickname = base.join("meshfolder").join(".ipc.sock");
        assert!(
            member_of(base, &no_nickname).is_none(),
            "an empty nickname would sweep every file of the folder"
        );
        let other_kind = base.join("meshfolder").join("one-two.state.json");
        assert!(
            member_of(base, &other_kind).is_none(),
            "only a control socket names a member"
        );
    }

    #[test]
    fn a_dead_member_loses_its_socket_and_chunks_and_keeps_a_fresh_log() {
        let base = scratch("dead");
        let dir = base.join("meshfolder");
        std::fs::create_dir_all(dir.join("one-two.chunks").join("maps")).expect("writable");
        let socket = dir.join("one-two.ipc.sock");
        stale_socket(&socket);
        let log = dir.join("one-two.tracing.log");
        std::fs::write(&log, "log").expect("writable");

        sweep_member(&base, &socket, SystemTime::now());

        assert!(!socket.exists(), "the stale socket belongs to no daemon");
        assert!(
            !dir.join("one-two.chunks").exists(),
            "the chunk folder is a cache"
        );
        assert!(log.exists(), "a fresh log is the record of the run");
        assert!(dir.exists(), "a folder that holds a file stays");

        let later = SystemTime::now() + DEAD_LOG_KEEP + Duration::from_secs(60);
        sweep_member(&base, &socket, later);
        assert!(!log.exists(), "the log goes when it is 7 days old");
        assert!(!dir.exists(), "the empty member folder goes");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_files_of_another_member_in_the_same_folder_stay() {
        let base = scratch("neighbour");
        let dir = base.join("meshfolder");
        std::fs::create_dir_all(&dir).expect("writable");
        let dead = dir.join("dead-one.ipc.sock");
        stale_socket(&dead);
        let neighbour_log = dir.join("live-two.tracing.log");
        std::fs::write(&neighbour_log, "log").expect("writable");

        let much_later = SystemTime::now() + DEAD_LOG_KEEP * 10;
        sweep_member(&base, &dead, much_later);

        assert!(
            neighbour_log.exists(),
            "a member that is not swept keeps its log"
        );
        assert!(dir.exists(), "its folder stays with it");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_link_in_the_place_of_a_chunk_folder_is_not_followed() {
        let base = scratch("link");
        let dir = base.join("meshfolder");
        let outside = base.join("outside");
        std::fs::create_dir_all(&dir).expect("writable");
        std::fs::create_dir_all(&outside).expect("writable");
        std::fs::write(outside.join("precious"), "x").expect("writable");
        symlink(&outside, dir.join("one-two.chunks")).expect("writable");
        let socket = dir.join("one-two.ipc.sock");
        stale_socket(&socket);

        sweep_member(&base, &socket, SystemTime::now());

        assert!(
            outside.join("precious").exists(),
            "the sweep removes only what the daemon owns"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn only_a_socket_that_nobody_listens_at_is_refused() {
        let base = scratch("refused");
        let stale = base.join("stale.ipc.sock");
        stale_socket(&stale);
        assert!(is_refused(&stale).await, "no listener: connection refused");
        assert!(
            is_refused(&base.join("gone.ipc.sock")).await,
            "no file: not found"
        );
        let live = base.join("live.ipc.sock");
        let listener = UnixListener::bind(&live).expect("writable");
        assert!(
            !is_refused(&live).await,
            "a listener accepts the connection"
        );
        drop(listener);
        let _ = std::fs::remove_dir_all(&base);
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
    fn the_daemon_locks_folder_of_the_base_is_never_swept() {
        let base = scratch("locks");
        let dir = base.join("meshfolder");
        let locks = base.join("locks");
        std::fs::create_dir_all(&dir).expect("writable");
        std::fs::create_dir_all(&locks).expect("writable");
        let lock = locks.join("meshfolder-one-two.lock");
        std::fs::write(&lock, "4242").expect("writable");
        let socket = dir.join("one-two.ipc.sock");
        stale_socket(&socket);
        let much_later = SystemTime::now() + DEAD_LOG_KEEP * 10;

        sweep_member(&base, &socket, much_later);
        sweep_orphan_logs(&base, much_later);

        assert!(!dir.exists(), "the member folder of a dead member goes");
        assert!(
            lock.exists(),
            "a lock file is never removed, so that two starts keep one lock"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_empty_member_folder_that_the_sweep_did_not_empty_stays() {
        let base = scratch("fresh-folder");
        // A starting daemon creates its folder before it writes its first file in it.
        let starting = base.join("startingmesh");
        std::fs::create_dir_all(&starting).expect("writable");
        let much_later = SystemTime::now() + DEAD_LOG_KEEP * 2;

        sweep_orphan_logs(&base, much_later);
        assert!(
            starting.exists(),
            "the orphan scan removes only a folder that it emptied"
        );

        let nothing_to_sweep = starting.join("someone.ipc.sock");
        sweep_member(&base, &nothing_to_sweep, much_later);
        assert!(
            starting.exists(),
            "a sweep that found nothing of the member leaves its folder alone"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_mesh_folder_that_is_a_link_is_not_swept_through() {
        let base = scratch("folder-link");
        let outside = base.join("outside");
        std::fs::create_dir_all(outside.join("one-two.chunks")).expect("writable");
        let socket_outside = outside.join("one-two.ipc.sock");
        stale_socket(&socket_outside);
        // \`active_socket_paths\` lists the sockets through a link, because it follows links.
        let mesh_folder = base.join("meshfolder");
        symlink(&outside, &mesh_folder).expect("writable");

        sweep_member(
            &base,
            &mesh_folder.join("one-two.ipc.sock"),
            SystemTime::now(),
        );

        assert!(
            socket_outside.exists(),
            "a socket behind a linked folder is not in the runtime base"
        );
        assert!(
            outside.join("one-two.chunks").exists(),
            "a chunk folder behind a linked folder is not in the runtime base"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
