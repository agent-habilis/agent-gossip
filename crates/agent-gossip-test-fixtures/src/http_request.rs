use std::io::{Read, Write};
use std::net::TcpStream;

/// One-shot HTTP/1.1 request against the local A2A binding; returns `(status, body)`.
///
/// The request is raw bytes over a `TcpStream`, with no client crate: the bytes are the contract.
///
/// # Panics
///
/// Panics if the connection fails, or if the response has no numeric status code.
#[must_use]
pub fn http_request(port: u16, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to a2a port");
    stream
        .write_all(request.as_bytes())
        .expect("the server stays up until the client has sent its request");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read response");
    let status_code = raw.split_whitespace().nth(1);
    let status: u16 = status_code
        .and_then(|code| code.parse().ok())
        .expect("a well-formed HTTP response starts with a status line carrying a numeric code");
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}
