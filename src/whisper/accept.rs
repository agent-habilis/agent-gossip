//! The accept side of the whisper plane: the `ProtocolHandler` for `WHISPER_ALPN`.
//! Each accepted stream carries an onion header; this hop peels its own layer
//! and either **forwards** (dials the next hop, writes the inner header, splices
//! the payload straight through) or **delivers** (terminal: hands the payload to
//! the same inbox a unicast frame lands in, so the destination ingests it
//! identically). A hop only ever sees ciphertext it can't interpret.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use iroh::endpoint::{Connection, Endpoint, RecvStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{EndpointAddr, EndpointId};
use tokio::sync::mpsc;
use x25519_dalek::StaticSecret;

use super::cell::peel;
use super::wire::{MAX_PAYLOAD_BYTES, read_header, write_header};
use super::{LOG_TARGET, WHISPER_ALPN};

#[derive(Clone)]
// `ProtocolHandler` requires `Debug`; hand-rolled so the X25519 secret never
// reaches a log.
pub(crate) struct WhisperAcceptor {
    /// Terminal delivery funnels here — the *same* inbox as unicast, so the
    /// destination ingests a whispered frame identically.
    inbox: mpsc::Sender<Bytes>,
    /// Our X25519 secret, to peel our onion layer.
    secret: Arc<StaticSecret>,
    /// The participant endpoint, to dial the next hop when forwarding. Filled by
    /// `daemon::setup` once the endpoint is bound (the acceptor is built first,
    /// so the Router can register it before the endpoint exists).
    endpoint: Arc<OnceLock<Endpoint>>,
}

impl std::fmt::Debug for WhisperAcceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperAcceptor").finish_non_exhaustive()
    }
}

impl WhisperAcceptor {
    pub(crate) fn new(
        inbox: mpsc::Sender<Bytes>,
        secret: StaticSecret,
        endpoint: Arc<OnceLock<Endpoint>>,
    ) -> Self {
        Self {
            inbox,
            secret: Arc::new(secret),
            endpoint,
        }
    }

    async fn handle_stream(&self, mut recv: RecvStream) {
        let header = match read_header(&mut recv).await {
            Ok(header) => header,
            Err(error) => {
                tracing::debug!(target: LOG_TARGET, %error, "circuit header read failed");
                return;
            }
        };
        let peeled = match peel(&header, &self.secret) {
            Ok(peeled) => peeled,
            Err(error) => {
                tracing::debug!(target: LOG_TARGET, %error, "circuit onion peel failed");
                return;
            }
        };
        match peeled.next {
            None => self.deliver_terminal(&mut recv).await,
            Some(next) => self.forward(next, peeled.inner, recv).await,
        }
    }

    /// Terminal hop: read the payload and hand it to the inbox (→ `gossip::ingest`).
    async fn deliver_terminal(&self, recv: &mut RecvStream) {
        match recv.read_to_end(MAX_PAYLOAD_BYTES).await {
            Ok(payload) => {
                if self.inbox.try_send(Bytes::from(payload)).is_err() {
                    tracing::debug!(target: LOG_TARGET, "whisper inbox full/closed; frame dropped");
                } else {
                    tracing::debug!(target: LOG_TARGET, "whisper circuit delivered a frame");
                }
            }
            Err(error) => {
                tracing::debug!(target: LOG_TARGET, %error, "circuit payload read failed");
            }
        }
    }

    /// Intermediate hop: dial the next hop, write the inner onion, splice payload.
    async fn forward(&self, next: EndpointId, inner: Option<String>, mut recv: RecvStream) {
        let Some(inner) = inner else {
            tracing::debug!(target: LOG_TARGET, "non-terminal cell missing inner onion");
            return;
        };
        let Some(endpoint) = self.endpoint.get() else {
            tracing::debug!(target: LOG_TARGET, "whisper endpoint not yet bound; dropping circuit");
            return;
        };
        let conn = match endpoint
            .connect(EndpointAddr::new(next), WHISPER_ALPN)
            .await
        {
            Ok(conn) => conn,
            Err(error) => {
                tracing::debug!(target: LOG_TARGET, %error, "dialing the next hop failed");
                return;
            }
        };
        let mut out = match conn.open_uni().await {
            Ok(out) => out,
            Err(error) => {
                tracing::debug!(target: LOG_TARGET, %error, "opening the next-hop stream failed");
                return;
            }
        };
        if write_header(&mut out, &inner).await.is_err() {
            return;
        }
        // Splice the remaining bytes (the payload) straight through, blind.
        let _ = tokio::io::copy(&mut recv, &mut out).await;
        let _ = out.finish();
        let _ = tokio::time::timeout(Duration::from_secs(2), out.stopped()).await;
    }
}

impl ProtocolHandler for WhisperAcceptor {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        // Each accepted uni-stream is an independent circuit hop; handle them
        // concurrently so one slow splice doesn't stall the next.
        while let Ok(recv) = conn.accept_uni().await {
            let this = self.clone();
            tokio::spawn(async move { this.handle_stream(recv).await });
        }
        Ok(())
    }
}
