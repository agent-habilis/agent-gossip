use crate::http_request;

/// One GET against the local A2A binding; returns `(status, body)`.
#[must_use]
pub fn http_get(port: u16, path: &str) -> (u16, String) {
    http_request(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
}
