//! End-to-end subprocess test for `ahsw port listen` / `connect` multiplexing:
//! two local TCP services exposed on one forward, mapped to two distinct local
//! ports on the same host, driven at once through the real CLI.
//!
//! This is a *behavioral* multi-port proof — both ports forward correctly and
//! simultaneously through the shipped binary. The stronger "exactly one QUIC
//! connection" invariant isn't black-box observable and is covered in-process
//! by `two_ports_share_one_quic_connection` in `src/port/produce.rs`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Instant;

mod common;
use common::{CONNECT_TIMEOUT, Node, POLL, test_cmd};

/// A spawned `ahsw port` child that is killed when the test ends (or panics).
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn a `Command` with its stdout piped, streaming each line onto a channel
/// via a reader thread (so the caller never blocks the child's stdout buffer).
fn spawn_piped(mut cmd: Command) -> (ChildGuard, Receiver<String>) {
    let mut child = cmd
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn ahsw port process");
    let stdout = child.stdout.take().expect("child stdout handle");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(text) => {
                    if tx.send(text).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    (ChildGuard(child), rx)
}

/// Wait (up to `CONNECT_TIMEOUT`) for a stdout line containing `needle`.
fn recv_line_containing(rx: &Receiver<String>, needle: &str) -> Option<String> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        match rx.recv_timeout(POLL) {
            Ok(line) if line.contains(needle) => return Some(line),
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

/// A local TCP service on an ephemeral port: accept one connection, read a
/// fixed-length `request`, reply `response`, and hand the request back. This is
/// what the producer dials as its `port listen` target.
fn spawn_target(request: &'static [u8], response: &'static [u8]) -> (u16, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock target");
    let port = listener.local_addr().expect("mock target addr").port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("mock target accept");
        let mut got = vec![0u8; request.len()];
        stream
            .read_exact(&mut got)
            .expect("mock target read request");
        stream
            .write_all(response)
            .expect("mock target write response");
        got
    });
    (port, handle)
}

/// An ephemeral free port (bind `:0`, read the port, release it).
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("ephemeral addr")
        .port()
}

/// Connect to a forwarded local port, retrying until the consumer's listener is
/// accepting (its bind is confirmed via stdout before we call this).
fn connect_with_retry(port: u16) -> TcpStream {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => return stream,
            Err(_) if Instant::now() < deadline => thread::sleep(POLL),
            Err(error) => panic!("could not connect to forwarded port {port}: {error}"),
        }
    }
}

#[test]
fn two_ports_forward_over_one_loopback_port_forward() {
    // A loopback swarm — its id carries loopback lookups (no mDNS/DHT/relay), so
    // the forward stays on this host. The daemon itself isn't consulted; the
    // forward dials the ticket's embedded address directly. `_creator` is held
    // so its Drop kills the daemon at test end.
    let (_creator, swarm) = Node::create_named("port-mux");

    // Two local services the producer exposes, each with a distinct reply.
    let (r1, target_a) = spawn_target(b"PING-A", b"PONG-A");
    let (r2, target_b) = spawn_target(b"PING-B", b"PONG-B");

    // Producer: expose r1 and r2. `--output json` prints exactly one line — the
    // ready-to-run `ahsw port connect <ticket> r1 r2` command.
    let (r1s, r2s) = (r1.to_string(), r2.to_string());
    let mut producer_cmd = test_cmd();
    producer_cmd.args([
        "port",
        "listen",
        "--swarm",
        swarm.as_str(),
        r1s.as_str(),
        r2s.as_str(),
        "--output",
        "json",
    ]);
    let (_producer, producer_rx) = spawn_piped(producer_cmd);
    let ticket_line = recv_line_containing(&producer_rx, "port connect")
        .expect("producer never printed a port connect command");
    let ticket = ticket_line
        .split_whitespace()
        .nth(3)
        .expect("port connect line missing ticket token")
        .to_string();

    // Distinct local ports so the consumer's listeners don't collide with the
    // producer's target services on this single host.
    let mut l1 = free_port();
    while [r1, r2].contains(&l1) {
        l1 = free_port();
    }
    let mut l2 = free_port();
    while [r1, r2, l1].contains(&l2) {
        l2 = free_port();
    }

    // Consumer: map l1→r1 and l2→r2. Human mode prints one `🐝 swarm:` status
    // line per bound listener; two of them means both listeners are up.
    let (map1, map2) = (format!("{l1}:{r1}"), format!("{l2}:{r2}"));
    let mut consumer_cmd = test_cmd();
    consumer_cmd.args([
        "port",
        "connect",
        ticket.as_str(),
        map1.as_str(),
        map2.as_str(),
    ]);
    let (_consumer, consumer_rx) = spawn_piped(consumer_cmd);
    recv_line_containing(&consumer_rx, "swarm:").expect("consumer never bound its first port");
    recv_line_containing(&consumer_rx, "swarm:").expect("consumer never bound its second port");

    // Drive both ports at once — both flows ride the single shared connection.
    let mut client_a = connect_with_retry(l1);
    let mut client_b = connect_with_retry(l2);
    client_a.write_all(b"PING-A").expect("write to l1");
    client_b.write_all(b"PING-B").expect("write to l2");
    let mut resp_a = [0u8; 6];
    let mut resp_b = [0u8; 6];
    client_a.read_exact(&mut resp_a).expect("read from l1");
    client_b.read_exact(&mut resp_b).expect("read from l2");

    assert_eq!(&resp_a, b"PONG-A", "l1 must reach the r1 service and back");
    assert_eq!(&resp_b, b"PONG-B", "l2 must reach the r2 service and back");
    assert_eq!(
        target_a.join().expect("target A thread"),
        b"PING-A",
        "the r1 service received its forwarded request"
    );
    assert_eq!(
        target_b.join().expect("target B thread"),
        b"PING-B",
        "the r2 service received its forwarded request"
    );
}
