use std::os::unix::net::UnixStream;

use crate::socket_path;

/// Send one raw JSON command line straight to a daemon's Unix socket and
/// return the (trimmed) response line — the wire-contract path, bypassing the
/// `agent-gossip` client entirely (so a test can exercise a single daemon-side
/// long-poll park, which `poll --long` deliberately hides behind its
/// re-issue loop).
///
/// # Panics
///
/// Panics if the socket connect, write, or read fails.
#[must_use]
pub fn ipc_raw(mesh: &str, nickname: &str, line: &str) -> String {
    use std::io::{BufRead, BufReader, Write};
    let mut stream =
        UnixStream::connect(socket_path(mesh, nickname)).expect("connect to daemon socket");
    stream
        .write_all(format!("{line}\n").as_bytes())
        .expect("write IPC command");
    stream.flush().expect("flush IPC command");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read IPC response");
    response.trim().to_string()
}
