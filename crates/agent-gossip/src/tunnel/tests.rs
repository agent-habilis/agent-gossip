//! End-to-end bridge tests over the real loopback iroh path: stand up an
//! exposer + a consumer and drive HTTP through the tunnel, asserting the card
//! is rewritten, bytes round-trip, a bad token is rejected, and the 1:1
//! invariant holds.

use std::sync::atomic::AtomicBool;

use futures_util::stream::FuturesUnordered;
use habilis_network::iroh::endpoint::ConnectionError;
use habilis_network::net::{add_peer_addr, address_book, build_peer_endpoint};
use habilis_network::protocol::{LookupOpts, Password};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::A2A_ALPN;
use super::a2a_ticket::A2aTicket;
use super::bridge::{Bridge, LocalSide, SharedConnection, forward_one};
use super::expose_params::{bind, serve_connection};
use super::local_gate::{LocalGate, TOKEN_HEADER};

/// A local HTTP origin that answers every connection with the same
/// `Content-Length`-framed response. Returns its `host:port`.
async fn spawn_origin(response: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("port zero asks the OS for a free port, so the bind cannot clash");
    let port = listener
        .local_addr()
        .expect("a bound listener always has a local socket address")
        .port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let response = response.clone();
            tokio::spawn(async move {
                let mut scratch = [0u8; 2048];
                let _ = sock.read(&mut scratch).await;
                let _ = sock.write_all(&response).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("127.0.0.1:{port}")
}

/// A minimal Agent Card HTTP response with the given absolute `url`.
fn card_response(url: &str) -> Vec<u8> {
    let body =
        format!(r#"{{"protocolVersion":"0.3.0","name":"origin","url":"{url}","skills":[]}}"#);
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

/// Stand up a loopback exposer forwarding to `origin`, serving with the shared
/// 1:1 slot. Returns the exposer endpoint and its ticket.
async fn spawn_exposer(
    origin: String,
    password: Option<Password>,
) -> (habilis_network::iroh::Endpoint, A2aTicket) {
    let (endpoint, ticket, auth) = bind(LookupOpts::loopback(), password.as_ref())
        .await
        .expect("a loopback endpoint needs no network, so the bind cannot fail");
    let accept_endpoint = endpoint.clone();
    tokio::spawn(async move {
        use futures_util::StreamExt as _;

        let paired = AtomicBool::new(false);
        let mut connections = FuturesUnordered::new();
        loop {
            tokio::select! {
                accepted = accept_endpoint.accept() => {
                    let Some(incoming) = accepted else { break };
                    let auth = auth.clone();
                    connections.push(async {
                        let _ = serve_connection(incoming, &origin, auth, &paired).await;
                    });
                }
                Some(()) = connections.next() => {}
            }
        }
    });
    (endpoint, ticket)
}

/// The bridge's own local base in these tests, and the `Host` a legitimate
/// client therefore sends.
const LOCAL_BASE: &str = "http://127.0.0.1:7777";
const LOCAL_AUTHORITY: &str = "127.0.0.1:7777";

fn bridge_for<'shared>(
    shared: &'shared SharedConnection,
    ticket: &A2aTicket,
    gate: LocalGate,
) -> Bridge<'shared> {
    Bridge::new(
        ticket,
        None,
        shared,
        LocalSide {
            base: LOCAL_BASE.to_owned(),
            gate,
        },
    )
}

/// Drive one HTTP request through `forward_one` (a local listener stands in for
/// the connect side's per-connection listener) and return the response.
async fn drive_gated(bridge: &Bridge<'_>, request: &[u8]) -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind local");
    let address = listener
        .local_addr()
        .expect("a bound listener always has a local socket address");
    let request = request.to_vec();
    let client = tokio::spawn(async move {
        let mut stream = TcpStream::connect(address).await.expect("connect local");
        stream
            .write_all(&request)
            .await
            .expect("the server side stays open until the request is fully sent");
        stream.shutdown().await.expect("shutdown client write");
        let mut got = Vec::new();
        stream.read_to_end(&mut got).await.expect("read response");
        got
    });
    let (accepted, _) = listener.accept().await.expect("accept local client");
    forward_one(bridge, accepted)
        .await
        .expect("the bridge is wired to a live origin, so one request goes through");
    client.await.expect("client task")
}

/// Drive a request carrying the gate's token, so the case under test is the
/// bridging itself rather than the gate.
async fn drive(
    shared: &SharedConnection,
    ticket: &A2aTicket,
    method_and_path: &str,
    extra: &str,
) -> Vec<u8> {
    let (gate, token) = LocalGate::new(LOCAL_AUTHORITY.to_owned(), false);
    let token = token.expect("a gated bridge mints a token");
    let request = format!(
        "{method_and_path} HTTP/1.1\r\nHost: {LOCAL_AUTHORITY}\r\n{TOKEN_HEADER}: {token}\r\n{extra}\r\n"
    );
    drive_gated(&bridge_for(shared, ticket, gate), request.as_bytes()).await
}

async fn consumer(ticket: &A2aTicket) -> (habilis_network::iroh::Endpoint, SharedConnection) {
    let endpoint = build_peer_endpoint(&ticket.lookups)
        .await
        .expect("the ticket lookups are loopback, so the endpoint needs no network");
    add_peer_addr(&address_book(&endpoint), ticket.address.clone());
    let shared = SharedConnection::new(endpoint.clone(), ticket.address.clone());
    (endpoint, shared)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_rewrites_the_agent_card_url_to_the_local_base() {
    let origin = spawn_origin(card_response("http://origin.internal:9999/a2a")).await;
    let (exposer, ticket) = spawn_exposer(origin, None).await;
    let (consumer_endpoint, shared) = consumer(&ticket).await;

    let response = drive(&shared, &ticket, "GET /.well-known/agent-card.json", "").await;

    let text = String::from_utf8_lossy(&response);
    assert!(
        text.contains(&format!(
            "{q}url{q}:{q}http://127.0.0.1:7777/a2a{q}",
            q = '"'
        )),
        "the card url must be rewritten to the local base: {text}"
    );

    consumer_endpoint.close().await;
    exposer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_forwards_a_non_card_exchange_byte_for_byte() {
    // A plain-text 200 is not a card — it must arrive unchanged.
    let response =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nok".to_vec();
    let origin = spawn_origin(response.clone()).await;
    let (exposer, ticket) = spawn_exposer(origin, None).await;
    let (consumer_endpoint, shared) = consumer(&ticket).await;

    let got = drive(&shared, &ticket, "POST /message", "Content-Length: 0\r\n").await;
    assert_eq!(got, response, "non-card response forwarded verbatim");

    consumer_endpoint.close().await;
    exposer.close().await;
}

/// The local port hands out the ticket-holder's access, so it needs a gate of
/// its own.
///
/// `forward_one` presents `auth.token` itself: whoever opens the local TCP port
/// drives A2A against the remote peer with the ticket-holder's credential. That
/// is any other process on the machine, and — because a `fetch` with a
/// CORS-safelisted content type fires no preflight — any page the user visits
/// while the bridge is up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_anonymous_local_client_never_reaches_the_origin() {
    let origin = spawn_origin(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 6\r\n\r\nsecret".to_vec(),
    )
    .await;
    let (exposer, ticket) = spawn_exposer(origin, None).await;
    let (consumer_endpoint, shared) = consumer(&ticket).await;

    let (gate, _token) = LocalGate::new(LOCAL_AUTHORITY.to_owned(), false);
    let got = drive_gated(
        &bridge_for(&shared, &ticket, gate),
        b"POST /message HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nContent-Length: 0\r\n\r\n",
    )
    .await;

    let text = String::from_utf8_lossy(&got);
    assert!(
        !text.contains("secret"),
        "an untokened request reached the origin: {text}"
    );
    assert!(
        text.starts_with("HTTP/1.1 403"),
        "expected a 403 from the bridge itself, got: {text}"
    );

    consumer_endpoint.close().await;
    exposer.close().await;
}

/// The browser path, end to end: a page's `fetch` carries `Origin` and cannot
/// omit it, so it is refused before a byte crosses the tunnel — with or without
/// the token, which is what makes `--allow-anonymous` safe against a web page
/// even though it drops the token check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cross_origin_request_is_refused_even_anonymously() {
    let origin = spawn_origin(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 6\r\n\r\nsecret".to_vec(),
    )
    .await;
    let (exposer, ticket) = spawn_exposer(origin, None).await;
    let (consumer_endpoint, shared) = consumer(&ticket).await;

    let (gate, token) = LocalGate::new(LOCAL_AUTHORITY.to_owned(), true);
    assert!(token.is_none(), "--allow-anonymous mints no token");
    let request = format!(
        "POST /message HTTP/1.1\r\nHost: {LOCAL_AUTHORITY}\r\nOrigin: https://evil.example\r\nContent-Type: text/plain\r\nContent-Length: 0\r\n\r\n"
    );
    let got = drive_gated(&bridge_for(&shared, &ticket, gate), request.as_bytes()).await;

    let text = String::from_utf8_lossy(&got);
    assert!(
        !text.contains("secret"),
        "a cross-origin request reached the origin: {text}"
    );
    assert!(
        text.starts_with("HTTP/1.1 403"),
        "expected a 403 from the bridge itself, got: {text}"
    );

    consumer_endpoint.close().await;
    exposer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_secret_is_rejected_on_a_passworded_bridge() {
    let origin = spawn_origin(card_response("http://origin:9999/a2a")).await;
    let password = Password::new("hunter2".to_owned());
    let (exposer, ticket) = spawn_exposer(origin, Some(password)).await;
    assert!(ticket.password, "the ticket must carry the password flag");
    let (consumer_endpoint, _shared) = consumer(&ticket).await;

    // Impostor: present the raw ticket secret (what a directory ad leaks)
    // instead of the password-stretched token.
    let connection = consumer_endpoint
        .connect(ticket.address.clone(), A2A_ALPN)
        .await
        .expect("dial");
    let (mut send, mut recv) = connection.open_bi().await.expect("open stream");
    send.write_all(ticket.secret.as_bytes())
        .await
        .expect("write token");
    let _ = recv.read_to_end(64).await;
    let closed = connection.close_reason();
    assert!(
        matches!(
            closed,
            Some(ConnectionError::ApplicationClosed(ref frame)) if u64::from(frame.error_code) == 3
        ),
        "raw secret must be closed with the wrong-password code, got {closed:?}"
    );

    consumer_endpoint.close().await;
    exposer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_consumer_is_refused_while_the_bridge_is_paired() {
    let origin = spawn_origin(card_response("http://origin:9999/a2a")).await;
    let (exposer, ticket) = spawn_exposer(origin, None).await;

    // Consumer A drives a request — that guarantees the exposer accepted and
    // claimed the 1:1 slot. Keeping A's endpoint + connection alive holds it.
    let (endpoint_a, shared_a) = consumer(&ticket).await;
    let _ = drive(&shared_a, &ticket, "GET /", "").await;
    let conn_a = shared_a.get().await.expect("A's connection stays live");
    assert!(conn_a.close_reason().is_none(), "A holds the pairing");

    // Consumer B: a second connection must be closed with BRIDGE_BUSY (4).
    let (endpoint_b, _shared_b) = consumer(&ticket).await;
    let conn_b = endpoint_b
        .connect(ticket.address.clone(), A2A_ALPN)
        .await
        .expect("B dial");
    let closed = conn_b.closed().await;
    assert!(
        matches!(
            closed,
            ConnectionError::ApplicationClosed(ref frame) if u64::from(frame.error_code) == 4
        ),
        "a second consumer must be refused with the bridge-busy code, got {closed:?}"
    );

    endpoint_a.close().await;
    endpoint_b.close().await;
    exposer.close().await;
}
