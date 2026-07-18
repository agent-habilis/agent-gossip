//! Deferred log sink. The subscriber is built before argv is parsed,
//! so logs are buffered in memory until `attach` learns the mesh id +
//! nickname, then the per-member file (socket-parity name) is opened
//! truncating, the buffer flushed, and writes pass through. A run that
//! never attaches (transient `msg`/`poll`/`mcp`, or startup failing
//! before identity) flushes to stderr instead — diagnostics are never
//! lost, memory never grows unbounded.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::util::logs::log_file_path;

use crate::protocol::{MeshId, Nickname};

/// Pending-buffer ceiling. `create`/`join` attach within sub-second (a
/// few KB); a long non-attaching process (`mcp`) hits this and flips
/// to stderr write-through — bounded memory, no file.
const LOG_BUF_CAP: usize = 1 << 20;

enum State {
    Pending(Vec<u8>),
    /// Writing to the per-member file. `written`/`max` bound its size:
    /// at the cap the file rotates to `<path>.1` (see [`rotate`]). `max
    /// == 0` disables rotation.
    Attached {
        file: fs::File,
        path: PathBuf,
        written: u64,
        max: u64,
    },
    Stderr,
}

/// Rotate `path` → `<path>.1` (overwriting any prior backup) and reopen
/// `path` truncating. Bounds disk to `2 × max` per member while keeping
/// the most recent ≥`max` bytes of history.
fn rotate(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut backup = path.as_os_str().to_owned();
    backup.push(".1");
    let _ = fs::rename(path, PathBuf::from(backup));
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        // Owner-only: the log records the mesh's nicknames, peers and (with
        // `--log-raw`) message bodies, so it must not be world-readable.
        .mode(0o600)
        .open(path)
}

/// Rotate the attached file once `written` reaches `max` (`max == 0`
/// disables rotation). Best-effort: on rotate failure keep the current file
/// (temporarily over cap) and retry next write — never drop the sink.
fn maybe_rotate(file: &mut fs::File, path: &Path, written: &mut u64, max: u64) {
    if max == 0 || *written < max {
        return;
    }
    if let Ok(rotated) = rotate(path) {
        *file = rotated;
        *written = 0;
    }
}

#[derive(Clone)]
pub struct LogSink(Arc<Mutex<State>>);

impl std::fmt::Debug for LogSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("LogSink").finish_non_exhaustive()
    }
}

static SINK: OnceLock<LogSink> = OnceLock::new();

/// Build the sink and register it process-globally. Called once by
/// `main` before subscriber init.
pub fn install() -> LogSink {
    let sink = LogSink(Arc::new(Mutex::new(State::Pending(Vec::new()))));
    let _ = SINK.set(sink.clone());
    sink
}

/// Identity resolved: open `<mesh-prefix>/<nick>.tracing.log` (truncate),
/// flush the buffer, pass through after. First-attach-wins — already
/// `Attached`/`Stderr` is a no-op.
pub fn attach(mesh: &MeshId, nickname: &Nickname) {
    let Some(sink) = SINK.get() else { return };
    sink.open(log_file_path(mesh.as_str(), nickname.as_str()));
}

impl LogSink {
    fn open(&self, path: PathBuf) {
        let mut state = self.0.lock().expect("log sink poisoned");
        if !matches!(*state, State::Pending(_)) {
            return;
        }
        // Validate the private base and create the log dir, failing closed: if a
        // squatted/symlinked base is detected, `ensure_parent_private` errors and
        // we fall through to the stderr path below rather than following the
        // symlink. A `--log-dir` override outside the base is not gated. (The log
        // is 0600, but a hijacked dir could still swap the file for an
        // attacker-owned one under create-truncate.)
        let opened = crate::util::ensure_parent_private(&path).and_then(|()| {
            use std::os::unix::fs::OpenOptionsExt as _;
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
        });
        match opened {
            Ok(mut file) => {
                let mut written = 0u64;
                if let State::Pending(buf) = &*state {
                    let _ = file.write_all(buf);
                    let _ = file.flush();
                    written = buf.len() as u64;
                }
                *state = State::Attached {
                    file,
                    path,
                    written,
                    max: crate::util::logs::log_max_bytes(),
                };
            }
            Err(error) => {
                eprintln!(
                    "warning: cannot open log file {}: {error}; logging to stderr",
                    path.display()
                );
                drain_to_stderr(&mut state);
            }
        }
    }
}

/// Process ending without ever attaching — a transient command
/// (`msg`/`poll`/`mcp`) or startup failing before identity. Flush
/// buffered diagnostics to stderr so they aren't lost.
/// # Panics
/// Panics if an internal invariant is violated.
pub fn flush_pending_to_stderr() {
    if let Some(sink) = SINK.get() {
        let mut state = sink.0.lock().expect("log sink poisoned");
        if matches!(*state, State::Pending(_)) {
            drain_to_stderr(&mut state);
        }
    }
}

fn drain_to_stderr(state: &mut State) {
    if let State::Pending(buf) = state {
        let _ = io::stderr().write_all(buf);
        let _ = io::stderr().flush();
    }
    *state = State::Stderr;
}

impl Write for LogSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.0.lock().expect("log sink poisoned");
        match &mut *state {
            State::Pending(buf) => {
                if buf.len() + bytes.len() > LOG_BUF_CAP {
                    let mut err = io::stderr();
                    let _ = err.write_all(buf);
                    let _ = err.write_all(bytes);
                    let _ = err.flush();
                    *state = State::Stderr;
                } else {
                    buf.extend_from_slice(bytes);
                }
            }
            State::Attached {
                file,
                path,
                written,
                max,
            } => {
                file.write_all(bytes)?;
                *written += bytes.len() as u64;
                maybe_rotate(file, path, written, *max);
            }
            State::Stderr => io::stderr().write_all(bytes)?,
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = self.0.lock().expect("log sink poisoned");
        match &mut *state {
            State::Attached { file, .. } => file.flush(),
            State::Stderr => io::stderr().flush(),
            State::Pending(_) => Ok(()),
        }
    }
}

// `make_writer` returns a cheap `Arc`-clone (no per-event heap alloc /
// dyn dispatch); interior mutability is the `Mutex<State>`.
impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogSink {
    type Writer = Self;
    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::{LogSink, State};

    #[test]
    fn attached_file_rotates_at_cap() {
        let dir = std::env::temp_dir().join(format!("agent-gossip-logsink-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("rot.log");
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .expect("open test log");
        let max = 100u64;
        let mut sink = LogSink(Arc::new(Mutex::new(State::Attached {
            file,
            path: path.clone(),
            written: 0,
            max,
        })));

        // Write well past the cap so at least one rotation fires.
        for _ in 0..10 {
            sink.write_all(&[b'x'; 30]).expect("write");
        }
        sink.flush().expect("flush");

        let active = fs::metadata(&path).expect("stat active").len();
        assert!(
            active < max,
            "active file must stay under the cap, got {active}"
        );
        let mut backup = path.as_os_str().to_owned();
        backup.push(".1");
        assert!(
            PathBuf::from(backup).exists(),
            "rotation backup `<path>.1` must exist"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
