//! The mDNS leg of the lookup layer: same-LAN multicast publish/resolve
//! of the seed-derived `rendezvous_id`. Wired only when `--mdns` is in
//! the allowlist; it accelerates same-LAN bootstrap and is one of the
//! independently-sufficient reliability layers (see the lookup glossary
//! in AGENTS.md).

use iroh::address_lookup::MdnsAddressLookup;
use iroh::endpoint::Builder;

/// Wire the LAN mDNS address-lookup onto a public-endpoint builder.
pub(super) fn wire(builder: Builder) -> Builder {
    tracing::debug!("mDNS address-lookup wired (LAN multicast publish + resolve)");
    builder.address_lookup(MdnsAddressLookup::builder())
}
