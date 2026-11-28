use std::path::{Path, PathBuf};

use anyhow::Result;
use habilis_network::util::process;
use habilis_network::util::process_start::ProcessStart;

use super::args::SessionFileOpts;
use super::session;

pub(super) fn session_file(options: &SessionFileOpts) -> Result<()> {
    let path = session_file_path(&crate::runtime_base(), options.session_pid)?;
    println!("{}", path.display());
    Ok(())
}

/// The name is a function of the pid and the start time of that process, read once from the kernel. A later
/// process that gets the same pid has another start time, so it gets another file, and no rule has to
/// tell the two apart. A pid with no live process names no file: there is no fallback to the pid alone.
fn session_file_path(base: &Path, pid: u32) -> Result<PathBuf> {
    let pid = session::session_anchor(Some(pid), None)?;
    let start = match process::start_of(pid) {
        ProcessStart::Live(start) => start,
        ProcessStart::Gone => {
            anyhow::bail!("no live process has pid {pid}, so no session file is named")
        }
        ProcessStart::Unknown(error) => anyhow::bail!(
            "the system gave no start time for pid {pid}, so no session file is named: {error}"
        ),
    };
    Ok(base.join("sessions").join(session_file_name(pid, start)))
}

fn session_file_name(pid: u32, start: u64) -> String {
    format!("{pid}-{start}.json")
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::{session_file_name, session_file_path};

    #[test]
    fn a_pid_with_another_start_time_gets_another_name() {
        assert_ne!(
            session_file_name(4242, 1_000),
            session_file_name(4242, 2_000),
            "a reused pid is another process, so it must not name the file of the first"
        );
    }

    #[test]
    fn init_and_a_pid_with_no_process_name_no_file() {
        let base = Path::new("/tmp");
        let mut gone = Command::new("sleep")
            .arg("0")
            .spawn()
            .expect("sleep is on PATH on every unix host");
        let dead_pid = gone.id();
        gone.wait().expect("a finished child can be waited on");

        assert!(session_file_path(base, 0).is_err(), "pid 0 owns no session");
        assert!(
            session_file_path(base, 1).is_err(),
            "pid 1 is init, an ancestor of every process"
        );
        assert!(
            session_file_path(base, dead_pid).is_err(),
            "a pid with no process names no file"
        );
    }

    #[test]
    fn a_live_process_names_a_file_in_the_sessions_folder() {
        let base = Path::new("/tmp/base");
        let pid = std::process::id();

        let first = session_file_path(base, pid)
            .expect("the test process runs, so its start time can be read");
        let second = session_file_path(base, pid)
            .expect("the test process runs, so its start time can be read");

        assert_eq!(first, second, "one process instance has one name");
        assert!(first.starts_with("/tmp/base/sessions"), "{first:?}");
        assert_eq!(
            first.extension().and_then(|text| text.to_str()),
            Some("json"),
            "a state file is a JSON file"
        );
    }
}
