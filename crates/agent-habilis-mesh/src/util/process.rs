/// Bound on the parent-walk in [`ancestry_contains`]: real process trees are
/// far shallower, and the bound keeps a pathological ppid cycle (pid reuse
/// mid-walk) from looping forever.
const MAX_ANCESTRY_DEPTH: usize = 64;

#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "libc::proc_pidinfo FFI; no safe wrapper exists")]
fn bsdinfo_for(pid: u32) -> Option<libc::proc_bsdinfo> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdinfo>() };
    let size = i32::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    let written =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    // A short read means the pid vanished mid-call or the struct layout
    // disagrees — either way the data is not trustworthy.
    (written == size).then_some(info)
}

#[cfg(target_os = "macos")]
#[must_use]
pub fn parent_of(pid: u32) -> Option<u32> {
    Some(bsdinfo_for(pid)?.pbi_ppid)
}

#[cfg(target_os = "macos")]
#[must_use]
pub fn comm_of(pid: u32) -> Option<String> {
    let info = bsdinfo_for(pid)?;
    let bytes: Vec<u8> = info
        .pbi_comm
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| u8::from_ne_bytes(byte.to_ne_bytes()))
        .collect();
    String::from_utf8(bytes).ok()
}

#[cfg(target_os = "linux")]
pub(crate) fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The comm field is parenthesized and may itself contain spaces and `)`,
    // so fields are only positional after the *last* `)`.
    let after_comm = stat.rsplit_once(')')?.1;
    let mut fields = after_comm.split_whitespace();
    let _run_state = fields.next()?;
    fields.next()?.parse().ok()
}

#[cfg(target_os = "linux")]
pub(crate) fn comm_of(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end().to_string())
}

/// Whether a process with this pid exists (regardless of whether we could
/// signal it: `EPERM` still proves existence).
#[expect(
    unsafe_code,
    reason = "libc::kill(pid, 0) FFI; no safe existence probe exists"
)]
#[must_use]
pub fn is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let status = unsafe { libc::kill(pid, 0) };
    status == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Ask a process to terminate (SIGTERM) — the graceful path: a mesh daemon
/// catches it, broadcasts `left`, and removes its state file.
#[expect(unsafe_code, reason = "libc::kill FFI; no safe wrapper")]
/// # Errors
/// Returns an error if the inputs are invalid or the operation fails.
pub fn terminate(pid: u32) -> std::io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let status = unsafe { libc::kill(pid, libc::SIGTERM) };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Whether `ancestor` appears in `pid`'s parent chain (proper ancestors
/// only). This is the session-ownership test: a daemon belongs to an agent
/// session iff the session's process is one of its ancestors. Never relax
/// this to "shares an ancestor" — every process shares init/launchd, and
/// two sessions in one terminal share the login shell.
#[must_use]
pub fn ancestry_contains(pid: u32, ancestor: u32) -> bool {
    let mut current = pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        let Some(parent) = parent_of(current) else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        if parent <= 1 {
            return false;
        }
        current = parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{ancestry_contains, comm_of, is_alive, parent_of, terminate};

    fn self_pid() -> u32 {
        std::process::id()
    }

    #[test]
    fn parent_of_self_is_our_ppid() {
        #[expect(unsafe_code, reason = "libc::getppid FFI; always succeeds")]
        let ppid = unsafe { libc::getppid() };
        assert_eq!(parent_of(self_pid()), u32::try_from(ppid).ok());
    }

    #[test]
    fn ancestry_contains_own_parent() {
        let parent = parent_of(self_pid()).expect("self has a parent");
        assert!(ancestry_contains(self_pid(), parent));
    }

    #[test]
    fn ancestry_excludes_self() {
        assert!(!ancestry_contains(self_pid(), self_pid()));
    }

    #[test]
    fn is_alive_self_true_absurd_false() {
        assert!(is_alive(self_pid()));
        assert!(!is_alive(u32::MAX - 1));
    }

    #[test]
    fn comm_of_self_names_the_test_binary() {
        let comm = comm_of(self_pid()).expect("self has a comm");
        assert!(!comm.is_empty());
    }

    #[test]
    fn terminate_rejects_a_dead_pid() {
        assert!(terminate(u32::MAX - 1).is_err());
    }
}
