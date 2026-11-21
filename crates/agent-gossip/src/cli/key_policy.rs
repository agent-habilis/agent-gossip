use std::fs::{OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};

use anyhow::Result;
use habilis_network::util::process;

/// What the CLI does with the kept member key of a start that has no `--owner-pid`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum KeyPolicy {
    /// A path that the user chose: the engine reuses the key.
    Chosen,
    /// The pid in the name of a session file is an ancestor of the daemon: the key is bound to it.
    Bound(u32),
    /// A session file that no ancestor owns: the kept key is forgotten and the start is a new member.
    /// The pid that the name gives, when it gives one.
    Fresh(Option<u32>),
}

/// What `forget_kept_key` found, and did.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum KeptFiles {
    NothingKept,
    Removed,
    /// A daemon holds the key lock, so the files are not the CLI's to remove.
    DaemonHoldsIt,
}

/// Whether `state_file` is a session file: a `.json` file that is a direct child of `<base>/sessions`. The
/// folder is the session location of the CLI, the path that the skills use. A path with a `..` part is
/// never taken for one.
fn is_session_file(state_file: &Path, base: &Path) -> bool {
    let plain = !state_file
        .components()
        .any(|part| matches!(part, Component::ParentDir));
    let json = state_file.extension().is_some_and(|ext| ext == "json");
    let in_sessions = state_file
        .parent()
        .is_some_and(|folder| same_folder(folder, &base.join("sessions")));
    plain && in_sessions && json
}

/// Whether two paths name one folder. Real folders are compared, so that another spelling of the runtime base
/// (`/private/tmp` for `/tmp` on macOS) does not turn the rule off: a safety rule that a spelling disables
/// protects nothing. Where a folder does not exist yet, the paths are compared as written.
fn same_folder(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(real_left), Ok(real_right)) => real_left == real_right,
        _ => left == right,
    }
}

/// The pid that the name of a session file gives: the whole stem is digits.
fn named_pid(state_file: &Path) -> Option<u32> {
    let stem = state_file.file_stem()?.to_str()?;
    let digits = !stem.is_empty() && stem.bytes().all(|byte| byte.is_ascii_digit());
    digits.then(|| stem.parse().ok()).flatten()
}

pub(super) fn key_policy(
    state_file: Option<&Path>,
    base: &Path,
    daemon: u32,
    is_ancestor: impl Fn(u32) -> bool,
) -> KeyPolicy {
    let Some(state_file) = state_file.filter(|path| is_session_file(path, base)) else {
        return KeyPolicy::Chosen;
    };
    let named = named_pid(state_file);
    match named {
        Some(pid) if pid > 1 && pid != daemon && is_ancestor(pid) => KeyPolicy::Bound(pid),
        other => KeyPolicy::Fresh(other),
    }
}

/// Forget the kept member key of `state_file`: remove its `.key`, `.run` and `.run.tmp` files, so that the
/// engine makes a new member at run 1. The CLI holds `<state>.key.lock` while it removes them, so a daemon
/// that runs on the path keeps its files, and the engine refuses that start as it does today. A link is
/// never followed: only a regular file is removed, and a lock file that is a link is an error.
///
/// # Errors
/// The lock file is a link, or the files cannot be opened or removed.
pub(super) fn forget_kept_key(state_file: &Path) -> io::Result<KeptFiles> {
    // No folder holds no key, and the CLI makes no folder for a state file: the engine decides, as before.
    let folder_is_missing = state_file.parent().is_some_and(|folder| {
        matches!(std::fs::metadata(folder), Err(error) if error.kind() == io::ErrorKind::NotFound)
    });
    if folder_is_missing {
        return Ok(KeptFiles::NothingKept);
    }
    let lock_path = state_file.with_extension("key.lock");
    if is_link(&lock_path) {
        return Err(io::Error::other(format!(
            "the lock file {} is a link, so it cannot keep two starts apart",
            lock_path.display()
        )));
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(false).mode(0o600);
    let lock = options.open(&lock_path)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(KeptFiles::DaemonHoldsIt),
        Err(TryLockError::Error(error)) => return Err(error),
    }
    let mut key_was_kept = false;
    for extension in ["key", "run", "run.tmp"] {
        let path = state_file.with_extension(extension);
        if is_regular_file(&path) {
            std::fs::remove_file(&path)?;
            key_was_kept |= extension == "key";
        }
    }
    Ok(if key_was_kept {
        KeptFiles::Removed
    } else {
        KeptFiles::NothingKept
    })
}

fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

/// Decide what the kept member key of this start means, and act on it. The flag \`--owner-pid\` wins: with it
/// nothing is decided here. Returns the pid that the key is bound to, when a session file names an ancestor
/// of this daemon. A session file that no ancestor owns starts as a new member and prints one warning on
/// stderr, when a kept key was dropped.
///
/// # Errors
/// The kept key of a session file cannot be forgotten.
pub(super) fn prepare_member_key(
    state_file: Option<&Path>,
    owner_pid: Option<u32>,
) -> Result<Option<u32>> {
    if owner_pid.is_some() {
        return Ok(None);
    }
    let daemon = std::process::id();
    let policy = key_policy(state_file, &crate::runtime_base(), daemon, |pid| {
        process::ancestry_contains(daemon, pid)
    });
    match (policy, state_file) {
        (KeyPolicy::Bound(pid), _) => Ok(Some(pid)),
        (KeyPolicy::Fresh(named), Some(path)) => {
            if forget_kept_key(path)? == KeptFiles::Removed {
                let reason = named.map_or_else(
                    || "the state file names no pid".to_owned(),
                    |pid| format!("the state file names pid {pid}, which is not an ancestor of this daemon"),
                );
                crate::status::warn(&format!("{reason}: a new member identity was made"));
            }
            Ok(None)
        }
        (KeyPolicy::Chosen | KeyPolicy::Fresh(_), _) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    use super::{KeptFiles, KeyPolicy, forget_kept_key, key_policy};

    const BASE: &str = "/run/agent-gossip-501";

    fn policy(state: &str, ancestors: &[u32]) -> KeyPolicy {
        key_policy(Some(Path::new(state)), Path::new(BASE), 4000, |pid| {
            ancestors.contains(&pid)
        })
    }

    #[test]
    fn a_path_that_is_not_a_session_file_is_chosen() {
        let outside = [
            "/tmp/runs/node.json",
            "/run/agent-gossip-501/node.json",
            "/run/agent-gossip-501/mesh/12.json",
            "/run/agent-gossip-501/sessions/sub/12.json",
            "/run/agent-gossip-501/sessions/12.key",
            "/run/agent-gossip-501/sessions/../12.json",
            "/other/agent-gossip-501/sessions/12.json",
        ];
        for path in outside {
            assert_eq!(
                policy(path, &[12]),
                KeyPolicy::Chosen,
                "{path} is a place that the user chose"
            );
        }
        assert_eq!(
            key_policy(None, Path::new(BASE), 4000, |_| true),
            KeyPolicy::Chosen,
            "no state file is the default path, which the engine owns"
        );
    }

    #[test]
    fn a_session_file_named_for_an_ancestor_is_bound_to_it() {
        assert_eq!(
            policy("/run/agent-gossip-501/sessions/12.json", &[12]),
            KeyPolicy::Bound(12),
            "the pid in the name is an ancestor of the daemon"
        );
    }

    #[test]
    fn a_session_file_that_no_ancestor_owns_is_fresh() {
        let cases = [
            // a pid that is not in the list of ancestors: live or dead
            (
                "/run/agent-gossip-501/sessions/13.json",
                KeyPolicy::Fresh(Some(13)),
            ),
            // init, the null pid and the daemon itself are never owners
            (
                "/run/agent-gossip-501/sessions/1.json",
                KeyPolicy::Fresh(Some(1)),
            ),
            (
                "/run/agent-gossip-501/sessions/0.json",
                KeyPolicy::Fresh(Some(0)),
            ),
            (
                "/run/agent-gossip-501/sessions/4000.json",
                KeyPolicy::Fresh(Some(4000)),
            ),
            // a name with no pid
            (
                "/run/agent-gossip-501/sessions/abc.json",
                KeyPolicy::Fresh(None),
            ),
            (
                "/run/agent-gossip-501/sessions/12x.json",
                KeyPolicy::Fresh(None),
            ),
            (
                "/run/agent-gossip-501/sessions/12.state.json",
                KeyPolicy::Fresh(None),
            ),
        ];
        for (path, expected) in cases {
            // 12, 1, 0 and 4000 all count as ancestors here: only the rule of the function refuses them
            assert_eq!(policy(path, &[12, 1, 0, 4000]), expected, "{path}");
        }
        assert_eq!(
            policy("/run/agent-gossip-501/sessions/12.json", &[]),
            KeyPolicy::Fresh(Some(12)),
            "a pid that is no ancestor"
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("session-key-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the temp dir is writable");
        dir
    }

    fn kept(state: &Path) {
        for extension in ["json", "key", "run", "run.tmp"] {
            fs::write(state.with_extension(extension), "x").expect("the temp dir is writable");
        }
    }

    #[test]
    fn forgetting_removes_the_key_and_the_run_counter_and_nothing_else() {
        let dir = scratch("forget");
        let state = dir.join("12.json");
        kept(&state);
        fs::write(dir.join("13.key"), "another session").expect("the temp dir is writable");

        let result = forget_kept_key(&state).expect("the files are the user's own");

        assert_eq!(result, KeptFiles::Removed, "a kept key was there");
        for extension in ["key", "run", "run.tmp"] {
            assert!(
                !state.with_extension(extension).exists(),
                "the {extension} file of the path is gone"
            );
        }
        assert!(state.exists(), "the state file is the daemon's to write");
        assert!(dir.join("13.key").exists(), "another path keeps its key");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgetting_where_nothing_is_kept_says_so() {
        let dir = scratch("nothing");
        let state = dir.join("12.json");
        assert_eq!(
            forget_kept_key(&state).expect("a folder that the user owns"),
            KeptFiles::NothingKept,
            "no key, no warning"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_daemon_that_holds_the_key_lock_keeps_its_files() {
        let dir = scratch("held");
        let state = dir.join("12.json");
        kept(&state);
        let lock =
            File::create(state.with_extension("key.lock")).expect("the temp dir is writable");
        lock.lock().expect("the lock is free");

        let result = forget_kept_key(&state).expect("a held lock is a result");

        assert_eq!(
            result,
            KeptFiles::DaemonHoldsIt,
            "a live daemon owns the files"
        );
        assert!(state.with_extension("key").exists(), "its key stays");
        assert!(
            state.with_extension("run").exists(),
            "its run counter stays"
        );
        drop(lock);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_link_in_the_place_of_a_file_is_not_followed() {
        let dir = scratch("links");
        let state = dir.join("12.json");
        let precious = dir.join("precious");
        fs::write(&precious, "not the key").expect("the temp dir is writable");
        symlink(&precious, state.with_extension("key")).expect("the temp dir is writable");

        forget_kept_key(&state).expect("a link is skipped, not an error");

        assert!(precious.exists(), "the target of a link stays");
        assert!(
            fs::symlink_metadata(state.with_extension("key")).is_ok(),
            "the link itself stays: only a regular file is the key"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lock_file_that_is_a_link_is_refused() {
        let dir = scratch("lock-link");
        let state = dir.join("12.json");
        let elsewhere = dir.join("elsewhere");
        fs::write(&elsewhere, "x").expect("the temp dir is writable");
        symlink(&elsewhere, state.with_extension("key.lock")).expect("the temp dir is writable");
        kept(&state);

        assert!(
            forget_kept_key(&state).is_err(),
            "a link cannot keep two starts apart, and the engine refuses it too"
        );
        assert!(state.with_extension("key").exists(), "nothing was removed");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_linked_spelling_of_the_runtime_base_is_still_a_session_file() {
        let real = scratch("real-base");
        fs::create_dir_all(real.join("sessions")).expect("the temp dir is writable");
        let link = scratch("link-base-holder").join("linked");
        symlink(&real, &link).expect("the temp dir is writable");

        // the state file is spelled through the link, and the base is the real folder
        let state = link.join("sessions").join("12.json");
        let policy = key_policy(Some(&state), &real, 4000, |pid| pid == 12);
        assert_eq!(
            policy,
            KeyPolicy::Bound(12),
            "another spelling of the same folder is the same folder"
        );
        let other = link.join("sessions").join("13.json");
        assert_eq!(
            key_policy(Some(&other), &real, 4000, |_| false),
            KeyPolicy::Fresh(Some(13)),
            "a rule that a spelling turns off would protect nothing"
        );
        let _ = fs::remove_dir_all(&real);
    }

    #[test]
    fn a_folder_that_does_not_exist_holds_no_kept_key() {
        let dir = scratch("missing-parent");
        let state = dir.join("not-yet").join("12.json");

        assert_eq!(
            forget_kept_key(&state).expect("a missing folder is not an error"),
            KeptFiles::NothingKept,
            "no folder, no key: the engine decides what a start there means"
        );
        assert!(
            !dir.join("not-yet").exists(),
            "the CLI does not make the folder of a state file"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
