#[cfg(windows)]
use std::fs;
#[cfg(unix)]
use std::os::unix::net::UnixStream;

use crate::socket_path;

/// Send one raw JSON command line straight to a daemon's IPC endpoint (its Unix socket, or its named pipe on Windows) and return the (trimmed) response line.
///
/// This is the wire-contract path, bypassing the
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
    #[cfg(unix)]
    let mut stream =
        UnixStream::connect(socket_path(mesh, nickname)).expect("connect to daemon socket");
    // On Windows the marker holds the pipe name, and a pipe opens as a file.
    #[cfg(windows)]
    let mut stream = {
        let marker = fs::read_to_string(socket_path(mesh, nickname))
            .expect("a bound daemon has written the marker that names its pipe");
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true);
        options
            .open(marker.trim_end())
            .expect("the pipe that the marker names accepts a client of this user")
    };
    stream
        .write_all(format!("{line}\n").as_bytes())
        .expect("write IPC command");
    stream.flush().expect("flush IPC command");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("the daemon writes one reply line before it closes the socket");
    response.trim().to_string()
}
