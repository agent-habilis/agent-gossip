//! The three `unstable-custom-transports` trait impls that make multihop a real
//! iroh transport.
//!
//! - [`MultihopTransport`] ([`CustomTransport`]) is the factory iroh calls at
//!   endpoint-bind time; it hands out the one [`MultihopEndpoint`].
//! - [`MultihopEndpoint`] ([`CustomEndpoint`]) surfaces terminally-delivered
//!   packets to iroh via `poll_recv` and publishes our own dialable address.
//! - [`MultihopSender`] ([`CustomSender`]) packs each outbound QUIC packet into a
//!   cell and hands it to the [`Forwarder`] for its first hop.

use std::io;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use iroh::endpoint::transports::{CustomEndpoint, CustomSender, CustomTransport, RecvInfo, Transmit};
use iroh_base::CustomAddr;
use tokio::sync::mpsc;

use crate::MULTIHOP_TRANSPORT_ID;
use crate::addr::{Route, RouteHop};
use crate::underlay::{Delivered, Forwarder};
use crate::wire::Cell;

/// State shared by the transport, its endpoint, and its senders.
#[derive(Debug)]
pub(crate) struct Shared {
    /// Our own hop — app-layer id plus underlay dial address — stamped as the
    /// `source` of every cell we originate so destinations can reply.
    pub(crate) self_hop: RouteHop,
    /// Our advertised address: a one-hop route straight to us. A direct
    /// underlay neighbour can dial it as-is; longer routes are computed by the
    /// initiator's address lookup.
    pub(crate) self_addr: CustomAddr,
    pub(crate) forwarder: Arc<Forwarder>,
}

/// The [`CustomTransport`] factory. Built once by [`crate::MultihopHandle`]; iroh
/// calls [`bind`](CustomTransport::bind) during `Endpoint::builder().bind()`.
#[derive(Debug)]
pub(crate) struct MultihopTransport {
    shared: Arc<Shared>,
    /// Taken by the single `bind` call. `None` afterwards.
    inbound: Mutex<Option<mpsc::Receiver<Delivered>>>,
}

impl MultihopTransport {
    pub(crate) fn new(shared: Arc<Shared>, inbound: mpsc::Receiver<Delivered>) -> Self {
        Self {
            shared,
            inbound: Mutex::new(Some(inbound)),
        }
    }
}

impl CustomTransport for MultihopTransport {
    fn bind(&self) -> io::Result<Box<dyn CustomEndpoint>> {
        let inbound = self
            .inbound
            .lock()
            .expect("inbound mutex poisoned")
            .take()
            .ok_or_else(|| io::Error::other("multihop transport already bound"))?;
        let local = n0_watcher::Watchable::new(vec![self.shared.self_addr.clone()]);
        Ok(Box::new(MultihopEndpoint {
            shared: Arc::clone(&self.shared),
            inbound,
            local,
        }))
    }
}

/// The [`CustomEndpoint`]: iroh's receive/local-address handle for the transport.
#[derive(Debug)]
struct MultihopEndpoint {
    shared: Arc<Shared>,
    inbound: mpsc::Receiver<Delivered>,
    local: n0_watcher::Watchable<Vec<CustomAddr>>,
}

impl CustomEndpoint for MultihopEndpoint {
    fn watch_local_addrs(&self) -> n0_watcher::Direct<Vec<CustomAddr>> {
        self.local.watch()
    }

    fn create_sender(&self) -> Arc<dyn CustomSender> {
        Arc::new(MultihopSender {
            shared: Arc::clone(&self.shared),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [io::IoSliceMut<'_>],
        metas: &mut [noq_udp::RecvMeta],
        recv_infos: &mut [RecvInfo],
    ) -> Poll<io::Result<usize>> {
        let cap = bufs.len().min(metas.len()).min(recv_infos.len());
        if cap == 0 {
            return Poll::Ready(Ok(0));
        }
        let mut batch = Vec::with_capacity(cap);
        match self.inbound.poll_recv_many(cx, &mut batch, cap) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(0) => return Poll::Ready(Err(io::Error::other("multihop inbound closed"))),
            Poll::Ready(_) => {}
        }
        let mut count = 0;
        for delivered in batch {
            let len = delivered.packet.len();
            if bufs[count].len() < len {
                // Packet larger than the buffer iroh handed us: drop it, as a
                // NIC would a jumbo frame on an MTU-limited path.
                continue;
            }
            bufs[count][..len].copy_from_slice(&delivered.packet);
            recv_infos[count] = RecvInfo::new(delivered.remote, Some(self.shared.self_addr.clone()));
            metas[count].len = len;
            metas[count].stride = len;
            count += 1;
        }
        if count > 0 {
            Poll::Ready(Ok(count))
        } else {
            // Everything we dequeued was undeliverable; ask to be polled again.
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// The [`CustomSender`]: routes each outbound QUIC packet onto its first hop.
#[derive(Debug)]
struct MultihopSender {
    shared: Arc<Shared>,
}

impl CustomSender for MultihopSender {
    fn is_valid_send_addr(&self, addr: &CustomAddr) -> bool {
        addr.id() == MULTIHOP_TRANSPORT_ID
    }

    fn poll_send(
        &self,
        _cx: &mut Context<'_>,
        dst: &CustomAddr,
        _src: Option<&CustomAddr>,
        transmit: &Transmit<'_>,
    ) -> Poll<io::Result<()>> {
        let Some(route) = Route::decode(dst) else {
            return Poll::Ready(Err(io::Error::other("undecodable multihop address")));
        };
        let hops = route.hops();
        let Some(first) = hops.first().cloned() else {
            return Poll::Ready(Err(io::Error::other("empty multihop route")));
        };
        let path = hops.to_vec();
        // A GSO batch is several datagrams concatenated; each is an independent
        // QUIC packet and rides its own cell.
        let segment = transmit.segment_size.unwrap_or(transmit.contents.len());
        for chunk in split_segments(transmit.contents, segment) {
            let cell = Cell {
                path: path.clone(),
                pos: 0,
                source: self.shared.self_hop.clone(),
                packet: chunk.to_vec(),
            };
            self.shared.forwarder.enqueue(&first, cell);
        }
        Poll::Ready(Ok(()))
    }
}

/// Split a GSO'd transmit into its constituent datagrams. An empty payload still
/// yields one (empty) datagram so a keep-alive is not silently dropped.
fn split_segments(contents: &[u8], segment: usize) -> Vec<&[u8]> {
    if contents.is_empty() {
        return vec![&contents[0..0]];
    }
    contents.chunks(segment.max(1)).collect()
}
