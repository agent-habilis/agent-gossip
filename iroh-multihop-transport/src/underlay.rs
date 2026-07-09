//! The forwarding **underlay**: a dedicated iroh endpoint that carries opaque
//! QUIC packets hop-by-hop between adjacent relays.
//!
//! The underlay is deliberately a *separate* endpoint from the application
//! endpoint whose packets it relays — the application endpoint is busy being the
//! end-to-end QUIC peer, and could not recursively carry itself. Adjacent hops
//! are link-state neighbours, so their underlay endpoints reach each other over a
//! normal direct/relay path.
//!
//! Outbound: one long-lived writer task per next-hop drains a channel of cells
//! onto a single uni-stream. Inbound ([`ForwardAcceptor`]): each accepted
//! uni-stream is read frame-by-frame; a cell is either forwarded to its next hop
//! or, if this node is the destination, delivered to the local transport.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_base::CustomAddr;
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use crate::addr::{Route, RouteHop};
use crate::wire::{Cell, read_cell, write_cell};

/// ALPN for the multihop underlay's hop-to-hop forwarding protocol.
pub(crate) const FORWARD_ALPN: &[u8] = b"iroh-multihop/forward/1";

/// Per-next-hop outbound cell queue depth. Dropping past this bounds memory the
/// way a UDP socket's send buffer does — QUIC treats the loss as any other.
const WRITER_QUEUE: usize = 1024;

/// Dial attempts before a writer task gives up on an unreachable next hop.
const DIAL_ATTEMPTS: usize = 4;

/// A packet that reached its destination on this node, ready for the local
/// transport's `poll_recv` to surface to iroh.
#[derive(Debug)]
pub(crate) struct Delivered {
    /// The sender's return address (a reversed route), reported to iroh as the
    /// remote of this packet so replies route back.
    pub(crate) remote: CustomAddr,
    pub(crate) packet: Vec<u8>,
}

/// Routes cells outbound to next hops and delivers terminal cells locally.
#[derive(Debug)]
pub(crate) struct Forwarder {
    underlay: Endpoint,
    runtime: Handle,
    /// One outbound writer channel per next-hop underlay endpoint id.
    writers: Mutex<HashMap<EndpointId, mpsc::Sender<Cell>>>,
    /// Terminal deliveries destined for the local application endpoint.
    inbound: mpsc::Sender<Delivered>,
}

impl Forwarder {
    pub(crate) fn new(underlay: Endpoint, inbound: mpsc::Sender<Delivered>) -> Self {
        Self {
            underlay,
            runtime: Handle::current(),
            writers: Mutex::new(HashMap::new()),
            inbound,
        }
    }

    /// Hand a cell to the writer for `hop`, spawning one if none is live. Never
    /// blocks: a full or dead queue drops the cell (QUIC recovers).
    pub(crate) fn enqueue(&self, hop: &RouteHop, cell: Cell) {
        let key = hop.underlay.id;
        // First attempt on any existing writer.
        if let Some(sender) = self.writer_for(key)
            && sender.try_send(cell.clone()).is_ok()
        {
            return;
        }
        // Stale or absent: (re)spawn a writer, then try once more.
        let sender = self.spawn_writer(hop.underlay.clone());
        let _ = sender.try_send(cell);
    }

    /// Deliver a cell that terminated here to the local transport.
    fn deliver(&self, cell: Cell) {
        let remote = Route::reverse_from(&cell.path, cell.source).encode();
        let delivered = Delivered {
            remote,
            packet: cell.packet,
        };
        // A full inbox drops the packet, as a saturated NIC would.
        let _ = self.inbound.try_send(delivered);
    }

    /// Process one received cell: forward to the next hop, or deliver if we are
    /// the destination.
    pub(crate) fn handle_cell(&self, cell: Cell) {
        match cell.next_hop() {
            Some(next) => {
                let next = next.clone();
                self.enqueue(&next, cell.advanced());
            }
            None => self.deliver(cell),
        }
    }

    fn writer_for(&self, key: EndpointId) -> Option<mpsc::Sender<Cell>> {
        let writers = self.writers.lock().expect("writers mutex poisoned");
        writers.get(&key).cloned()
    }

    /// Insert and spawn a fresh writer task for `dst`, replacing any dead entry.
    fn spawn_writer(&self, dst: EndpointAddr) -> mpsc::Sender<Cell> {
        let (tx, rx) = mpsc::channel(WRITER_QUEUE);
        {
            let mut writers = self.writers.lock().expect("writers mutex poisoned");
            writers.insert(dst.id, tx.clone());
        }
        let underlay = self.underlay.clone();
        self.runtime.spawn(writer_task(underlay, dst, rx));
        tx
    }
}

/// Drain `rx` onto a single uni-stream to `dst`. Exits (dropping its channel) on
/// dial failure or a write error, so the next `enqueue` respawns a fresh writer.
async fn writer_task(underlay: Endpoint, dst: EndpointAddr, mut rx: mpsc::Receiver<Cell>) {
    let Some(conn) = dial_with_retry(&underlay, &dst).await else {
        tracing::debug!(hop = %dst.id.fmt_short(), "multihop underlay dial gave up");
        return;
    };
    let mut send = match conn.open_uni().await {
        Ok(send) => send,
        Err(error) => {
            tracing::debug!(hop = %dst.id.fmt_short(), %error, "open underlay uni-stream failed");
            return;
        }
    };
    while let Some(cell) = rx.recv().await {
        if let Err(error) = write_cell(&mut send, &cell).await {
            tracing::debug!(hop = %dst.id.fmt_short(), %error, "underlay write failed; tearing down writer");
            break;
        }
    }
    let _ = send.finish();
}

async fn dial_with_retry(underlay: &Endpoint, dst: &EndpointAddr) -> Option<Connection> {
    for attempt in 0..DIAL_ATTEMPTS {
        match underlay.connect(dst.clone(), FORWARD_ALPN).await {
            Ok(conn) => return Some(conn),
            Err(error) => {
                tracing::trace!(hop = %dst.id.fmt_short(), attempt, %error, "underlay dial attempt failed");
                // Linear backoff; adjacent hops that are momentarily busy settle fast.
                tokio::time::sleep(Duration::from_millis(100 * (attempt as u64 + 1))).await;
            }
        }
    }
    None
}

/// The `FORWARD_ALPN` accept side: read cells off each incoming uni-stream and
/// hand them to the [`Forwarder`].
#[derive(Debug, Clone)]
pub(crate) struct ForwardAcceptor {
    forwarder: std::sync::Arc<Forwarder>,
}

impl ForwardAcceptor {
    pub(crate) fn new(forwarder: std::sync::Arc<Forwarder>) -> Self {
        Self { forwarder }
    }

    async fn read_loop(self, mut recv: iroh::endpoint::RecvStream) {
        while let Ok(cell) = read_cell(&mut recv).await {
            self.forwarder.handle_cell(cell);
        }
    }
}

impl ProtocolHandler for ForwardAcceptor {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        // Loop ends when the upstream hop closes the connection: normal teardown.
        while let Ok(recv) = connection.accept_uni().await {
            tokio::spawn(self.clone().read_loop(recv));
        }
        Ok(())
    }
}
