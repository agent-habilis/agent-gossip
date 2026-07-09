//! End-to-end proof that the multihop transport carries a *real* iroh
//! connection: two application endpoints that share **only** the multihop custom
//! transport (IP and relay cleared) still open a bidirectional QUIC stream and
//! echo bytes, with every packet relayed hop-by-hop through intermediate peers'
//! underlay endpoints.

use std::net::SocketAddr;
use std::time::Duration;

use iroh::endpoint::{Connection, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointId, RelayMode, SecretKey, TransportAddr};
use iroh_multihop_transport::{MULTIHOP_TRANSPORT_ID, MultihopHandle};

const ECHO_ALPN: &[u8] = b"multihop-test/echo/1";

/// Uniform link cost; the exact value is irrelevant when there is one route.
const COST: u32 = 10;

#[derive(Debug, Clone)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        tokio::io::copy(&mut recv, &mut send).await?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }
}

/// A plain loopback underlay endpoint: real IP on 127.0.0.1, no relay/discovery.
async fn underlay_endpoint() -> Endpoint {
    let loopback: SocketAddr = "127.0.0.1:0".parse().expect("loopback addr");
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr(loopback)
        .expect("valid bind addr")
        .bind()
        .await
        .expect("bind underlay endpoint")
}

/// A node that participates in the end-to-end connection: an application endpoint
/// whose only transport is multihop, plus its underlay + routing handle.
struct Node {
    app: Endpoint,
    handle: MultihopHandle,
    id: EndpointId,
}

async fn make_node(secret: SecretKey) -> Node {
    let underlay = underlay_endpoint().await;
    let handle = MultihopHandle::new(secret.public(), underlay);
    let app = Endpoint::builder(presets::Minimal)
        .secret_key(secret.clone())
        .relay_mode(RelayMode::Disabled)
        .preset(handle.clone())
        .clear_ip_transports()
        .clear_relay_transports()
        .bind()
        .await
        .expect("bind app endpoint");
    Node {
        app,
        handle,
        id: secret.public(),
    }
}

/// A pure relay: only an underlay + forwarding handle (no application endpoint).
/// Kept alive by the returned handle. Its app id is `secret.public()`.
async fn make_relay(secret: SecretKey) -> MultihopHandle {
    let underlay = underlay_endpoint().await;
    MultihopHandle::new(secret.public(), underlay)
}

fn secret(seed: u8) -> SecretKey {
    SecretKey::from_bytes(&[seed; 32])
}

/// Assert the connection's selected path is our multihop custom transport — i.e.
/// the bytes really travelled over the relays, not some direct shortcut.
fn assert_multihop_selected(conn: &Connection) {
    let selected_is_multihop = conn.paths().iter().any(|path| {
        path.is_selected()
            && matches!(path.remote_addr(), TransportAddr::Custom(addr) if addr.id() == MULTIHOP_TRANSPORT_ID)
    });
    assert!(
        selected_is_multihop,
        "expected the selected path to be the multihop transport"
    );
}

async fn echo_roundtrip(conn: &Connection, msg: &[u8]) {
    let (mut send, mut recv) = conn.open_bi().await.expect("open bi stream");
    send.write_all(msg).await.expect("write");
    send.finish().expect("finish");
    let echoed = recv.read_to_end(64).await.expect("read echo");
    assert_eq!(echoed, msg, "echo mismatch over multihop");
}

#[tokio::test]
async fn connects_end_to_end_through_one_relay() {
    // A ──underlay──> R ──underlay──> B, with A and B sharing only the multihop
    // transport. The whole QUIC handshake is relayed through R.
    let alice = make_node(secret(1)).await;
    let relay = make_relay(secret(2)).await;
    let relay_id = secret(2).public();
    let bob = make_node(secret(3)).await;

    // Alice's routing table: the chain A→R→B, each vector carrying that node's
    // real underlay dial address.
    alice
        .handle
        .feed_topology(alice.handle.link_vector(1, vec![(relay_id, COST)]));
    alice
        .handle
        .feed_topology(relay.link_vector(1, vec![(alice.id, COST), (bob.id, COST)]));
    alice
        .handle
        .feed_topology(bob.handle.link_vector(1, vec![(relay_id, COST)]));

    let echo = Router::builder(bob.app.clone())
        .accept(ECHO_ALPN, Echo)
        .spawn();

    let conn = tokio::time::timeout(Duration::from_secs(30), alice.app.connect(bob.id, ECHO_ALPN))
        .await
        .expect("connect timed out")
        .expect("connect over one relay");

    assert_multihop_selected(&conn);
    echo_roundtrip(&conn, b"hello over one hop").await;

    conn.close(0u32.into(), b"done");
    echo.shutdown().await.expect("shutdown echo router");
}

#[tokio::test]
async fn connects_end_to_end_through_two_relays() {
    // A → R1 → R2 → B: a genuine multi-hop route.
    let alice = make_node(secret(11)).await;
    let r1 = make_relay(secret(12)).await;
    let r1_id = secret(12).public();
    let r2 = make_relay(secret(13)).await;
    let r2_id = secret(13).public();
    let bob = make_node(secret(14)).await;

    alice
        .handle
        .feed_topology(alice.handle.link_vector(1, vec![(r1_id, COST)]));
    alice
        .handle
        .feed_topology(r1.link_vector(1, vec![(alice.id, COST), (r2_id, COST)]));
    alice
        .handle
        .feed_topology(r2.link_vector(1, vec![(r1_id, COST), (bob.id, COST)]));
    alice
        .handle
        .feed_topology(bob.handle.link_vector(1, vec![(r2_id, COST)]));

    let echo = Router::builder(bob.app.clone())
        .accept(ECHO_ALPN, Echo)
        .spawn();

    let conn = tokio::time::timeout(Duration::from_secs(30), alice.app.connect(bob.id, ECHO_ALPN))
        .await
        .expect("connect timed out")
        .expect("connect over two relays");

    assert_multihop_selected(&conn);
    echo_roundtrip(&conn, b"hello across two hops").await;

    conn.close(0u32.into(), b"done");
    echo.shutdown().await.expect("shutdown echo router");
}
