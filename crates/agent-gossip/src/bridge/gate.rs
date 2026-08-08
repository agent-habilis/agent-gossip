use std::fmt::Write as _;

use fofoca::protocol::ct_eq;

use super::SECRET_LEN;

/// Largest request head the gate will buffer before refusing. A head is a URL
/// and a handful of headers; anything approaching this is not a client.
pub(super) const MAX_HEAD_BYTES: usize = 32 * 1024;

/// The header a local client presents to prove it was told the bridge's token.
///
/// Deliberately not `Authorization`: the bridge is a transport, and the A2A
/// server behind the exposer may want that header for its own auth. A custom
/// name also means a browser cannot send it without a preflight, which fails.
pub(super) const TOKEN_HEADER: &str = "x-agent-gossip-token";

/// Why a local request was refused, as the reason line the client is told.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Refusal(pub(super) &'static str);

/// The local half of the bridge's access control.
///
/// The bridge presents the ticket credential on the client's behalf
/// (`connect::forward_one`), so an ungated port hands the ticket-holder's
/// access to whoever opens a TCP connection to it. That is any other process on
/// the machine, and any page the user visits while the bridge is up: a `fetch`
/// with a CORS-safelisted content type fires no preflight, so the write lands
/// even though the response is opaque.
pub(super) struct LocalGate {
    /// The `host:port` this bridge bound. Pinning `Host` to it is what defeats
    /// DNS rebinding — the trick that turns a page's blind cross-origin write
    /// into a readable exchange by making the attacker's origin match ours.
    authority: String,
    /// `None` under `--allow-anonymous`, which drops only the token check.
    token: Option<[u8; SECRET_LEN]>,
}

impl LocalGate {
    /// A gate for a bridge bound at `authority` (`host:port`), minting a fresh
    /// token unless `anonymous`. Returns the gate and the token to print, if any.
    pub(super) fn new(authority: String, anonymous: bool) -> (Self, Option<String>) {
        if anonymous {
            return (
                Self {
                    authority,
                    token: None,
                },
                None,
            );
        }
        let raw: [u8; SECRET_LEN] = rand::random();
        let hex = raw.iter().fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
        (
            Self {
                authority,
                token: Some(raw),
            },
            Some(hex),
        )
    }

    /// Screen one request head, returning it with the token header removed.
    ///
    /// `head` is everything up to (not including) the blank line that ends the
    /// head. The returned string is what gets forwarded, terminator included.
    pub(super) fn screen(&self, head: &str) -> Result<String, Refusal> {
        let mut lines = head.split("\r\n");
        let request_line = lines
            .next()
            .filter(|line| !line.is_empty())
            .ok_or(Refusal("malformed request"))?;
        let headers = lines
            .filter(|line| !line.is_empty())
            .map(|line| {
                line.split_once(':')
                    .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim(), line))
                    .ok_or(Refusal("malformed header"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let values_of = |wanted: &str| -> Vec<&str> {
            headers
                .iter()
                .filter(|(name, ..)| name == wanted)
                .map(|(_, value, _)| *value)
                .collect()
        };

        // No A2A SDK sends `Origin`; a browser always does on a cross-origin
        // request, and cannot suppress it. Its presence is the tell.
        if headers.iter().any(|(name, ..)| name == "origin") {
            return Err(Refusal("cross-origin requests are refused"));
        }
        // Two `Host` headers let a proxy and an origin disagree about the
        // target — request smuggling's opening move.
        let [host] = values_of("host")[..] else {
            return Err(Refusal("exactly one host header is required"));
        };
        if !self.host_matches(host) {
            return Err(Refusal("host does not match the bridge address"));
        }
        self.check_token(&values_of(TOKEN_HEADER))?;

        let mut kept = String::with_capacity(head.len());
        kept.push_str(request_line);
        kept.push_str("\r\n");
        // The token header is ours, not the origin's: dropped, never forwarded.
        for (_, _, line) in headers.iter().filter(|(name, ..)| name != TOKEN_HEADER) {
            kept.push_str(line);
            kept.push_str("\r\n");
        }
        kept.push_str("\r\n");
        Ok(kept)
    }

    fn check_token(&self, presented: &[&str]) -> Result<(), Refusal> {
        let Some(expected) = self.token.as_ref() else {
            return Ok(());
        };
        let [presented] = *presented else {
            return Err(if presented.is_empty() {
                Refusal("missing bridge token")
            } else {
                Refusal("duplicate token header")
            });
        };
        let presented = decode_token(presented).ok_or(Refusal("malformed bridge token"))?;
        if ct_eq(expected, &presented) {
            Ok(())
        } else {
            Err(Refusal("invalid bridge token"))
        }
    }

    /// `localhost` is the same machine by every resolver, so accepting it
    /// alongside the bound address costs nothing a rebinding attack can use.
    fn host_matches(&self, host: &str) -> bool {
        if host == self.authority {
            return true;
        }
        match (host.rsplit_once(':'), self.authority.rsplit_once(':')) {
            (Some(("localhost", port)), Some((_, expected))) => port == expected,
            _ => false,
        }
    }
}

fn decode_token(hex: &str) -> Option<[u8; SECRET_LEN]> {
    if hex.len() != SECRET_LEN * 2 {
        return None;
    }
    let mut out = [0u8; SECRET_LEN];
    for (slot, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let pair = std::str::from_utf8(pair).ok()?;
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

/// The refusal a screened-out client gets. `Connection: close` because the
/// gate has consumed an unknown amount of the request and will not read on.
pub(super) fn refusal_response(refusal: &Refusal) -> Vec<u8> {
    let body = format!("{{\"error\":\"{}\"}}", refusal.0);
    format!(
        "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::{LocalGate, Refusal, TOKEN_HEADER};

    fn gated() -> (LocalGate, String) {
        let (gate, token) = LocalGate::new("127.0.0.1:7777".to_owned(), false);
        (gate, token.expect("a gated bridge mints a token"))
    }

    fn head(extra: &str) -> String {
        format!("POST /message HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n{extra}")
    }

    #[test]
    fn a_request_with_the_token_passes_and_the_token_is_not_forwarded() {
        let (gate, token) = gated();
        let forwarded = gate
            .screen(&head(&format!(
                "{TOKEN_HEADER}: {token}\r\nContent-Length: 0"
            )))
            .expect("a tokened request passes");
        assert!(forwarded.starts_with("POST /message HTTP/1.1\r\n"));
        assert!(forwarded.contains("Content-Length: 0\r\n"));
        assert!(
            !forwarded.to_ascii_lowercase().contains(TOKEN_HEADER),
            "the bridge's own token must not reach the origin: {forwarded}"
        );
        assert!(forwarded.ends_with("\r\n\r\n"));
    }

    #[test]
    fn a_request_without_the_token_is_refused() {
        let (gate, _) = gated();
        assert_eq!(
            gate.screen(&head("Content-Length: 0")),
            Err(Refusal("missing bridge token"))
        );
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let (gate, _) = gated();
        let wrong = "00".repeat(32);
        assert_eq!(
            gate.screen(&head(&format!("{TOKEN_HEADER}: {wrong}"))),
            Err(Refusal("invalid bridge token"))
        );
        assert_eq!(
            gate.screen(&head(&format!("{TOKEN_HEADER}: nothex"))),
            Err(Refusal("malformed bridge token"))
        );
    }

    /// The browser half: a page's `fetch` always carries `Origin`, and it
    /// cannot omit it. Refusing on sight closes that path whatever the token
    /// setting, which is why anonymous mode drops only the token check.
    #[test]
    fn an_origin_header_is_refused_even_anonymously() {
        let (gate, token) = gated();
        assert_eq!(
            gate.screen(&head(&format!(
                "Origin: https://evil.example\r\n{TOKEN_HEADER}: {token}"
            ))),
            Err(Refusal("cross-origin requests are refused"))
        );

        let (anon, minted) = LocalGate::new("127.0.0.1:7777".to_owned(), true);
        assert!(minted.is_none(), "anonymous mode mints no token");
        assert!(
            anon.screen(&head("Content-Length: 0")).is_ok(),
            "anonymous mode still serves a plain client"
        );
        assert_eq!(
            anon.screen(&head("Origin: https://evil.example")),
            Err(Refusal("cross-origin requests are refused"))
        );
    }

    /// DNS rebinding: the page keeps its own origin and points the name at
    /// 127.0.0.1, so `Origin` looks same-origin and the response is readable.
    /// The `Host` it sends is the attacker's name, which is the tell.
    #[test]
    fn a_rebound_host_is_refused() {
        let (gate, token) = gated();
        let rebound = format!(
            "POST /message HTTP/1.1\r\nHost: evil.example:7777\r\n{TOKEN_HEADER}: {token}"
        );
        assert_eq!(
            gate.screen(&rebound),
            Err(Refusal("host does not match the bridge address"))
        );

        // `localhost` on the bridge's own port is the same machine.
        let localhost =
            format!("POST /message HTTP/1.1\r\nHost: localhost:7777\r\n{TOKEN_HEADER}: {token}");
        assert!(gate.screen(&localhost).is_ok());
    }

    #[test]
    fn a_head_without_exactly_one_host_is_refused() {
        let (gate, token) = gated();
        let none = format!("GET / HTTP/1.1\r\n{TOKEN_HEADER}: {token}");
        assert_eq!(
            gate.screen(&none),
            Err(Refusal("exactly one host header is required"))
        );
        let two = format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nHost: 127.0.0.1:7777\r\n{TOKEN_HEADER}: {token}"
        );
        assert_eq!(
            gate.screen(&two),
            Err(Refusal("exactly one host header is required"))
        );
    }
}
