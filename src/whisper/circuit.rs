//! The initiator side: build a telescoping circuit to a destination and push a
//! payload through it. One-way for v1 — the payload is delivered into the
//! destination's inbox (the same seam a unicast frame lands in).

use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::Endpoint;
use iroh::{EndpointAddr, EndpointId};

use super::WHISPER_ALPN;
use super::cell::{CircuitId, build_onion};
use super::wire::write_header;

/// Build a circuit along `path` — the ordered hops *after* us, each with its
/// X25519 public key, the last being the destination — and send `payload`
/// through it. `circuit_id` distinguishes concurrent attempts.
///
/// # Errors
/// Fails on an empty path, a failure to dial the first hop, or a stream write
/// error (a mid-circuit hop failure surfaces here as a reset/short write).
pub(crate) async fn open_circuit(
    endpoint: &Endpoint,
    circuit_id: CircuitId,
    path: &[(EndpointId, [u8; 32])],
    payload: &[u8],
) -> Result<()> {
    let onion = build_onion(circuit_id, path)?;
    let (first_hop, _) = path.first().context("empty circuit path")?;
    let conn = endpoint
        .connect(EndpointAddr::new(*first_hop), WHISPER_ALPN)
        .await
        .context("dialing the first whisper hop failed")?;
    let mut send = conn
        .open_uni()
        .await
        .context("opening the circuit stream failed")?;
    write_header(&mut send, &onion).await?;
    send.write_all(payload)
        .await
        .context("writing the circuit payload failed")?;
    send.finish()
        .context("finishing the circuit stream failed")?;
    // Let the far end drain before the connection drops (fast/loopback race).
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use iroh::protocol::Router;
    use tokio::sync::mpsc;
    use x25519_dalek::{PublicKey, StaticSecret};

    use super::open_circuit;
    use crate::lookup::{add_peer_addr, build_participant_endpoint};
    use crate::protocol::swarm::LookupOpts;
    use crate::util::tuning::WHISPER_MAX_PATHS;
    use crate::whisper::WHISPER_ALPN;
    use crate::whisper::accept::WhisperAcceptor;
    use crate::whisper::linkstate::{LinkStateStore, LinkVector};
    use crate::whisper::metric::LinkMetric;

    /// A hop/terminal node: an endpoint accepting `WHISPER_ALPN`, its X25519
    /// public key, and the inbox its terminal delivery lands in.
    struct WhisperNode {
        endpoint: iroh::Endpoint,
        seal_pub: [u8; 32],
        inbox: mpsc::Receiver<bytes::Bytes>,
    }

    async fn whisper_node(seed: u8) -> (WhisperNode, Router) {
        let endpoint = build_participant_endpoint(&LookupOpts::loopback())
            .await
            .expect("bind loopback endpoint");
        let secret = StaticSecret::from([seed; 32]);
        let seal_pub = PublicKey::from(&secret).to_bytes();
        let (tx, inbox) = mpsc::channel(8);
        let cell = Arc::new(OnceLock::new());
        let acceptor = WhisperAcceptor::new(tx, secret, cell.clone());
        let router = Router::builder(endpoint.clone())
            .accept(WHISPER_ALPN, acceptor)
            .spawn();
        let _ = cell.set(endpoint.clone());
        (
            WhisperNode {
                endpoint,
                seal_pub,
                inbox,
            },
            router,
        )
    }

    #[tokio::test]
    async fn circuit_whispers_a_payload_through_an_intermediate_hop() {
        // A (initiator) → R (whisperer, forwards) → B (terminal, delivers).
        let alice = build_participant_endpoint(&LookupOpts::loopback())
            .await
            .expect("bind alice");
        let (hop, _hop_router) = whisper_node(11).await;
        let (mut bob, _bob_router) = whisper_node(12).await;

        // Address book: A can dial R, R can dial B.
        add_peer_addr(&alice, hop.endpoint.addr()).expect("register R with A");
        add_peer_addr(&hop.endpoint, bob.endpoint.addr()).expect("register B with R");

        let path = vec![
            (hop.endpoint.id(), hop.seal_pub),
            (bob.endpoint.id(), bob.seal_pub),
        ];
        let payload = b"a directed frame over the circuit".to_vec();
        open_circuit(&alice, 7, &path, &payload)
            .await
            .expect("open circuit");

        let received = tokio::time::timeout(Duration::from_secs(15), bob.inbox.recv())
            .await
            .expect("delivery timed out")
            .expect("inbox closed");
        assert_eq!(received.as_ref(), payload.as_slice());

        alice.close().await;
    }

    #[tokio::test]
    async fn circuit_whispers_a_payload_through_two_intermediate_hops() {
        // A (initiator) → B (forwards) → C (forwards) → D (terminal, delivers).
        let alice = build_participant_endpoint(&LookupOpts::loopback())
            .await
            .expect("bind alice");
        let (bob, _bob_router) = whisper_node(21).await;
        let (carol, _carol_router) = whisper_node(22).await;
        let (mut dave, _dave_router) = whisper_node(23).await;

        // Address book along the chain: A→B, B→C, C→D.
        add_peer_addr(&alice, bob.endpoint.addr()).expect("register B with A");
        add_peer_addr(&bob.endpoint, carol.endpoint.addr()).expect("register C with B");
        add_peer_addr(&carol.endpoint, dave.endpoint.addr()).expect("register D with C");

        let path = vec![
            (bob.endpoint.id(), bob.seal_pub),
            (carol.endpoint.id(), carol.seal_pub),
            (dave.endpoint.id(), dave.seal_pub),
        ];
        let payload = b"a directed frame across two whisperers".to_vec();
        open_circuit(&alice, 9, &path, &payload)
            .await
            .expect("open circuit");

        let received = tokio::time::timeout(Duration::from_secs(15), dave.inbox.recv())
            .await
            .expect("delivery timed out")
            .expect("inbox closed");
        assert_eq!(received.as_ref(), payload.as_slice());

        alice.close().await;
    }

    #[tokio::test]
    async fn whisper_delivers_over_the_computed_best_path() {
        // Prove the two asks together: compute the optimal route from link-state,
        // then push data over exactly that route. Topology: chain A→B→C→D plus a
        // cheaper direct A→D — so the computed best path is the one-hop shortcut,
        // and the payload must arrive at D over it.
        let alice = build_participant_endpoint(&LookupOpts::loopback())
            .await
            .expect("bind alice");
        let (bob, _bob_router) = whisper_node(31).await;
        let (carol, _carol_router) = whisper_node(32).await;
        let (mut dave, _dave_router) = whisper_node(33).await;

        // Address book for every dialable edge, so whichever route is chosen dials.
        add_peer_addr(&alice, bob.endpoint.addr()).expect("register B with A");
        add_peer_addr(&alice, dave.endpoint.addr()).expect("register D with A");
        add_peer_addr(&bob.endpoint, carol.endpoint.addr()).expect("register C with B");
        add_peer_addr(&carol.endpoint, dave.endpoint.addr()).expect("register D with C");

        // Link-state built from the live nodes' real ids and seal keys. The chain
        // costs 5 per hop (15 total); the direct A→D costs 1, so it wins.
        let vector = |origin, seal_key, links: &[(iroh::EndpointId, u32)]| LinkVector {
            origin,
            seq: 1,
            seal_key,
            links: links
                .iter()
                .map(|(to, cost)| (*to, LinkMetric(*cost)))
                .collect(),
        };
        let mut store = LinkStateStore::default();
        // Alice is the source; its own seal key is never used to seal a hop.
        store.ingest(vector(
            alice.id(),
            [0u8; 32],
            &[(bob.endpoint.id(), 5), (dave.endpoint.id(), 1)],
        ));
        store.ingest(vector(
            bob.endpoint.id(),
            bob.seal_pub,
            &[(carol.endpoint.id(), 5)],
        ));
        store.ingest(vector(
            carol.endpoint.id(),
            carol.seal_pub,
            &[(dave.endpoint.id(), 5)],
        ));
        store.ingest(vector(dave.endpoint.id(), dave.seal_pub, &[]));

        let paths = store.circuit_paths(alice.id(), dave.endpoint.id(), WHISPER_MAX_PATHS);
        assert_eq!(
            paths[0].iter().map(|(hop, _)| *hop).collect::<Vec<_>>(),
            vec![dave.endpoint.id()],
            "the computed best path is the cheaper direct shortcut"
        );

        let payload = b"delivered over the computed best path".to_vec();
        open_circuit(&alice, 13, &paths[0], &payload)
            .await
            .expect("open circuit over the computed path");

        let received = tokio::time::timeout(Duration::from_secs(15), dave.inbox.recv())
            .await
            .expect("delivery timed out")
            .expect("inbox closed");
        assert_eq!(received.as_ref(), payload.as_slice());

        alice.close().await;
    }
}
