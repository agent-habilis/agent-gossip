//! The dial side of the unicast plane: a per-peer connection pool that dials
//! lazily, reuses a warm QUIC connection across messages, and re-warms after a
//! close. Modeled on `a2a::connect::SharedConnection`, but keyed per endpoint
//! and with a short dial budget so a cold peer fails toward gossip fast rather
//! than stalling the event loop.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use bytes::Bytes;
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tokio::sync::Mutex;

use super::{LOG_TARGET, UNICAST_ALPN};

/// How long a background warm-dial (or an inline unicast-only dial) keeps
/// trying before giving up. Deliberately short — far under a2a's 90s discovery
/// deadline — because a message that can't warm quickly should ride gossip now,
/// not wait. The addressee's `EndpointAddr` is already registered with the
/// endpoint (`add_peer_addr`), so a reachable peer resolves well inside this.
const DIAL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub(crate) struct UnicastPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    /// `None` for a detached pool (unit-test states / pre-wiring default): every
    /// operation is inert, so unicast simply never engages and sends ride gossip.
    endpoint: Option<Endpoint>,
    conns: Mutex<HashMap<EndpointId, Connection>>,
    /// Endpoints with an in-flight dial, so a burst of directed messages to a
    /// cold peer spawns exactly one dial instead of a stampede.
    dialing: Mutex<HashSet<EndpointId>>,
}

impl UnicastPool {
    /// A pool wired to `endpoint`, able to dial and carry unicast traffic.
    pub(crate) fn new(endpoint: Endpoint) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                endpoint: Some(endpoint),
                conns: Mutex::new(HashMap::new()),
                dialing: Mutex::new(HashSet::new()),
            }),
        }
    }

    /// A detached pool with no endpoint — every operation is a no-op. The
    /// default an [`EventLoopState`](crate::daemon::state::EventLoopState) holds
    /// until the real loop installs an endpoint-backed pool; also what the
    /// lower-level unit tests (which never run the transport) get.
    pub(crate) fn disconnected() -> Self {
        Self {
            inner: Arc::new(PoolInner {
                endpoint: None,
                conns: Mutex::new(HashMap::new()),
                dialing: Mutex::new(HashSet::new()),
            }),
        }
    }

    /// The underlying participant endpoint, when wired — used by the relay
    /// transport to dial the first circuit hop (and to learn our own id). `None`
    /// for a detached pool (unit-test states).
    pub(crate) fn endpoint(&self) -> Option<Endpoint> {
        self.inner.endpoint.clone()
    }

    /// Send `bytes` over a warm connection to `eid` if one exists, returning
    /// `true` on a fire-and-forget handoff. `false` means no warm connection —
    /// the caller then warms in the background and rides gossip for this
    /// message. The actual stream write is spawned so the event loop never
    /// blocks on QUIC I/O.
    pub(crate) async fn send_if_warm(&self, eid: EndpointId, bytes: Bytes) -> bool {
        let conn = {
            let conns = self.inner.conns.lock().await;
            match conns.get(&eid) {
                Some(conn) if conn.close_reason().is_none() => conn.clone(),
                _ => return false,
            }
        };
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            if let Err(error) = send_one(&conn, &bytes).await {
                tracing::debug!(target: LOG_TARGET, %error, "unicast send failed; dropping connection");
                inner.conns.lock().await.remove(&eid);
            }
        });
        true
    }

    /// Spawn a background dial to `eid` and pool the connection for the next
    /// message. At most one dial per endpoint is in flight at a time. Inert on a
    /// detached pool.
    pub(crate) fn warm(&self, eid: EndpointId) {
        let Some(endpoint) = self.inner.endpoint.clone() else {
            return;
        };
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            if !inner.dialing.lock().await.insert(eid) {
                return; // a dial to this peer is already in flight
            }
            let result = dial(&endpoint, eid).await;
            inner.dialing.lock().await.remove(&eid);
            match result {
                Ok(conn) => {
                    tracing::info!(target: LOG_TARGET, "unicast connection established");
                    inner.conns.lock().await.insert(eid, conn);
                }
                Err(error) => {
                    tracing::debug!(target: LOG_TARGET, %error, "unicast warm dial failed");
                }
            }
        });
    }

    /// Ensure a connection to `eid` (reusing a warm one or dialing inline) and
    /// send `bytes` over it, awaiting the handoff. Used only in unicast-only
    /// mode, where there is no gossip fallback to carry the first message.
    ///
    /// # Errors
    /// A detached pool, a dial that fails/times out, or a stream write error.
    pub(crate) async fn dial_and_send(&self, eid: EndpointId, bytes: Bytes) -> Result<()> {
        let Some(endpoint) = self.inner.endpoint.clone() else {
            bail!("unicast pool has no endpoint");
        };
        let warm = {
            let conns = self.inner.conns.lock().await;
            conns
                .get(&eid)
                .filter(|conn| conn.close_reason().is_none())
                .cloned()
        };
        let conn = if let Some(conn) = warm {
            conn
        } else {
            let conn = dial(&endpoint, eid).await?;
            self.inner.conns.lock().await.insert(eid, conn.clone());
            conn
        };
        if let Err(error) = send_one(&conn, &bytes).await {
            self.inner.conns.lock().await.remove(&eid);
            return Err(error);
        }
        Ok(())
    }
}

/// Dial `eid` on the unicast ALPN within [`DIAL_TIMEOUT`]. The endpoint already
/// knows the peer's address (registered via `add_peer_addr`), so a bare-id
/// `EndpointAddr` resolves through the endpoint's address book + lookups.
async fn dial(endpoint: &Endpoint, eid: EndpointId) -> Result<Connection> {
    match tokio::time::timeout(
        DIAL_TIMEOUT,
        endpoint.connect(EndpointAddr::new(eid), UNICAST_ALPN),
    )
    .await
    {
        Ok(Ok(conn)) => Ok(conn),
        Ok(Err(error)) => Err(anyhow::anyhow!("{error}")),
        Err(_) => bail!("unicast dial timed out after {DIAL_TIMEOUT:?}"),
    }
}

/// One message per unidirectional stream: open, write the whole frame, finish.
/// No length framing is needed — the accept side reads the stream to EOF and
/// gets exactly one serialized `Message`. The finished stream keeps flushing
/// after it is dropped because the connection stays pooled, so there is no need
/// to await `stopped()` (which would block the event loop on the inline
/// unicast-only path).
async fn send_one(conn: &Connection, bytes: &[u8]) -> Result<()> {
    let mut stream = conn.open_uni().await?;
    stream.write_all(bytes).await?;
    stream.finish()?;
    Ok(())
}
