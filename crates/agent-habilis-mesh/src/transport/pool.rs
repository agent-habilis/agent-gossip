//! The dial side of the unicast plane: a per-peer connection pool that dials
//! lazily, reuses a warm QUIC connection across messages, and re-dials after a
//! close. Modeled on the application bridge's shared connection, but keyed per endpoint
//! and with a short dial budget so a send to an unreachable peer fails fast
//! rather than stalling the event loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use bytes::Bytes;
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tokio::sync::Mutex;

use super::{LOG_TARGET, UNICAST_ALPN};

/// How long an inline dial keeps trying before giving up. Deliberately short —
/// far under the application's 90s discovery deadline — because the dial blocks the send,
/// and an unreachable peer should surface as an error now, not stall the
/// caller. The addressee's `EndpointAddr` is already registered with the
/// endpoint (`add_peer_addr`), so a reachable peer resolves well inside this.
const DIAL_TIMEOUT: Duration = Duration::from_secs(3);

/// After a failed dial the endpoint goes on cooldown: further cold sends to it
/// error immediately instead of re-dialing. The dial blocks the event loop, so
/// without this a burst of directed frames to one dead peer (a 16-shard repair
/// reply, a flushed backlog) serializes a [`DIAL_TIMEOUT`] stall per frame —
/// the cooldown bounds that to one stall per window per peer.
const DIAL_FAILURE_COOLDOWN: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct UnicastPool {
    inner: Arc<PoolInner>,
}

impl std::fmt::Debug for UnicastPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnicastPool").finish_non_exhaustive()
    }
}

struct PoolInner {
    /// `None` for a detached pool (unit-test states / pre-wiring default): every
    /// operation is inert, so a directed send simply errors instead of dialing.
    endpoint: Option<Endpoint>,
    conns: Mutex<HashMap<EndpointId, Connection>>,
    /// When each endpoint's last dial failed, for the
    /// [`DIAL_FAILURE_COOLDOWN`] gate. An entry clears on the first retry past
    /// the window or on a successful dial.
    dial_failures: Mutex<HashMap<EndpointId, Instant>>,
}

impl UnicastPool {
    /// A pool wired to `endpoint`, able to dial and carry unicast traffic.
    pub(crate) fn new(endpoint: Endpoint) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                endpoint: Some(endpoint),
                conns: Mutex::new(HashMap::new()),
                dial_failures: Mutex::new(HashMap::new()),
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
                dial_failures: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Send `bytes` over a warm connection to `eid` if one exists, returning
    /// `true` on a fire-and-forget handoff. `false` means no warm connection —
    /// the caller then dials inline via [`Self::dial_and_send`]. The actual
    /// stream write is spawned so the event loop never blocks on QUIC I/O
    /// (awaiting it wouldn't buy delivery truth anyway — QUIC buffers locally,
    /// so a write "succeeds" before the path is proven). A write that fails on
    /// a half-dead pooled connection evicts it and redials-and-resends once in
    /// the background, so the detectable failure mode recovers instead of
    /// silently losing the frame; a path that dies *after* buffering remains
    /// at-most-once (an app-level concern: waiter timeouts, anti-entropy).
    pub(crate) async fn send_if_warm(&self, eid: EndpointId, bytes: Bytes) -> bool {
        let conn = {
            let conns = self.inner.conns.lock().await;
            match conns.get(&eid) {
                Some(conn) if conn.close_reason().is_none() => conn.clone(),
                _ => return false,
            }
        };
        let pool = self.clone();
        tokio::spawn(async move {
            let Err(error) = send_one(&conn, &bytes).await else {
                return;
            };
            tracing::debug!(target: LOG_TARGET, %error, "unicast send failed; dropping connection and redialing");
            pool.inner.conns.lock().await.remove(&eid);
            if let Err(redial_error) = pool.dial_and_send(eid, bytes).await {
                tracing::debug!(target: LOG_TARGET, %redial_error, "redial after a failed warm send also failed; frame dropped");
            }
        });
        true
    }

    /// Ensure a connection to `eid` (reusing a warm one or dialing inline) and
    /// send `bytes` over it, awaiting the handoff. The cold path for every
    /// directed send — there is no other transport to carry the first message.
    ///
    /// # Errors
    /// A detached pool, an endpoint on failed-dial cooldown, a dial that
    /// fails/times out, or a stream write error.
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
            if on_cooldown(
                &mut *self.inner.dial_failures.lock().await,
                eid,
                Instant::now(),
            ) {
                bail!("unicast dial on cooldown after a recent failure");
            }
            match dial(&endpoint, eid).await {
                Ok(conn) => {
                    self.inner.dial_failures.lock().await.remove(&eid);
                    self.inner.conns.lock().await.insert(eid, conn.clone());
                    conn
                }
                Err(error) => {
                    self.inner
                        .dial_failures
                        .lock()
                        .await
                        .insert(eid, Instant::now());
                    return Err(error);
                }
            }
        };
        if let Err(error) = send_one(&conn, &bytes).await {
            self.inner.conns.lock().await.remove(&eid);
            return Err(error);
        }
        Ok(())
    }

    /// Drop `eid`'s pooled connection and dial-failure cooldown: the peer left
    /// gracefully, so neither may serve or block a future dial (a rejoin
    /// re-dials cold). The connection is closed explicitly — `Connection` is a
    /// cheap clone handle, and in-flight spawned writers hold clones, so just
    /// dropping the map entry would leave the QUIC connection open until every
    /// clone drops.
    pub(crate) async fn forget(&self, eid: EndpointId) {
        if let Some(conn) = self.inner.conns.lock().await.remove(&eid) {
            conn.close(0u32.into(), b"peer left");
        }
        self.inner.dial_failures.lock().await.remove(&eid);
    }
}

/// Whether `eid`'s last dial failed inside the [`DIAL_FAILURE_COOLDOWN`]
/// window. A stale entry (past the window) is cleared so the next dial runs.
fn on_cooldown(failures: &mut HashMap<EndpointId, Instant>, eid: EndpointId, now: Instant) -> bool {
    match failures.get(&eid) {
        Some(failed_at) if now.duration_since(*failed_at) < DIAL_FAILURE_COOLDOWN => true,
        Some(_) => {
            failures.remove(&eid);
            false
        }
        None => false,
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use iroh::{EndpointId, SecretKey};

    use super::{DIAL_FAILURE_COOLDOWN, on_cooldown};

    fn endpoint_id(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn a_fresh_failure_puts_the_endpoint_on_cooldown() {
        let mut failures = HashMap::new();
        let bob = endpoint_id(1);
        let now = Instant::now();
        failures.insert(bob, now);
        assert!(on_cooldown(&mut failures, bob, now));
        // Another endpoint is unaffected.
        assert!(!on_cooldown(&mut failures, endpoint_id(2), now));
    }

    #[test]
    fn an_expired_failure_clears_and_allows_the_dial() {
        let mut failures = HashMap::new();
        let bob = endpoint_id(1);
        let failed_at = Instant::now();
        failures.insert(bob, failed_at);
        let later = failed_at + DIAL_FAILURE_COOLDOWN + Duration::from_millis(1);
        assert!(!on_cooldown(&mut failures, bob, later));
        assert!(
            !failures.contains_key(&bob),
            "the stale entry is cleared, not retained"
        );
    }

    /// A graceful `Left` forgets the peer's pool slots: the dial cooldown is
    /// cleared so a rejoin dials immediately, and forgetting an unknown
    /// endpoint is a no-op. (Closing a warm `Connection` needs a live
    /// endpoint pair; that path is covered by the network suite.)
    #[tokio::test]
    async fn forget_clears_the_cooldown_and_tolerates_absence() {
        let pool = super::UnicastPool::disconnected();
        let bob = endpoint_id(1);
        pool.inner
            .dial_failures
            .lock()
            .await
            .insert(bob, Instant::now());

        pool.forget(bob).await;

        assert!(pool.inner.dial_failures.lock().await.is_empty());
        assert!(pool.inner.conns.lock().await.is_empty());
        // Absent endpoint: nothing to drop, nothing panics.
        pool.forget(endpoint_id(2)).await;
    }
}
