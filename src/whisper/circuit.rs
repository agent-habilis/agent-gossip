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
    use crate::whisper::WHISPER_ALPN;
    use crate::whisper::accept::WhisperAcceptor;

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
}
