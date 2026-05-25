//! The mainline-DHT leg of the lookup layer: operator-free, eternal
//! publish/resolve of the seed-derived `rendezvous_id` via pkarr on the
//! `BitTorrent` mainline DHT. Wired only when `--dht` is in the allowlist;
//! it is the slow-but-always-there backstop and one of the
//! independently-sufficient reliability layers (see the lookup glossary
//! in AGENTS.md).

use iroh::address_lookup::DhtAddressLookup;
use iroh::endpoint::Builder;

/// Wire the mainline-DHT address-lookup onto a public-endpoint builder.
pub(super) fn wire(builder: Builder) -> Builder {
    tracing::debug!("mainline DHT address-lookup wired (pkarr publish + resolve)");
    builder.address_lookup(DhtAddressLookup::builder())
}
