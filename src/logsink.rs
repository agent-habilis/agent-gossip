//! Deferred log sink. The subscriber is built before argv is parsed,
//! so logs are buffered in memory until `attach` learns the swarm id +
//! nickname, then the per-member file (socket-parity name) is opened
//! truncating, the buffer flushed, and writes pass through. A run that
//! never attaches (transient `msg`/`poll`/`mcp`, or startup failing
//! before identity) flushes to stderr instead — diagnostics are never
//! lost, memory never grows unbounded.

use std::fs;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, OnceLock};

use crate::protocol::{Nickname, SwarmId};
use crate::transport::ipc::log_file_path;

/// Pending-buffer ceiling. `create`/`join` attach within sub-second (a
/// few KB); a long non-attaching process (`mcp`) hits this and flips
/// to stderr write-through — bounded memory, no file.
const LOG_BUF_CAP: usize = 1 << 20;

enum State {
    Pending(Vec<u8>),
    Attached(fs::File),
    Stderr,
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
pub(crate) fn install() -> LogSink {
    let sink = LogSink(Arc::new(Mutex::new(State::Pending(Vec::new()))));
    let _ = SINK.set(sink.clone());
    sink
}

/// Identity resolved: open `<swarm_prefix>-<nick>.log` (truncate),
/// flush the buffer, pass through after. Open failure → stderr.
pub(crate) fn attach(swarm: &SwarmId, nickname: &Nickname) {
    let Some(sink) = SINK.get() else { return };
    let mut state = sink.0.lock().expect("log sink poisoned");
    if !matches!(*state, State::Pending(_)) {
        return;
    }
    let path = log_file_path(swarm, nickname);
    let opened = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| {
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)
        });
    match opened {
        Ok(mut file) => {
            if let State::Pending(buf) = &*state {
                let _ = file.write_all(buf);
                let _ = file.flush();
            }
            *state = State::Attached(file);
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

/// Process ending without ever attaching — a transient command
/// (`msg`/`poll`/`mcp`) or startup failing before identity. Flush
/// buffered diagnostics to stderr so they aren't lost.
pub(crate) fn flush_pending_to_stderr() {
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
            State::Attached(file) => file.write_all(bytes)?,
            State::Stderr => io::stderr().write_all(bytes)?,
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = self.0.lock().expect("log sink poisoned");
        match &mut *state {
            State::Attached(file) => file.flush(),
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
