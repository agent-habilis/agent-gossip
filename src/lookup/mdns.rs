//! The mDNS leg of the lookup layer: same-LAN multicast publish/resolve
//! of the seed-derived `rendezvous_id`. Wired only when `--mdns` is in
//! the allowlist; it accelerates same-LAN bootstrap and is one of the
//! independently-sufficient reliability layers (see the lookup glossary
//! in AGENTS.md).

use anyhow::{Context, Result};
use iroh::Endpoint;
use iroh_mdns_address_lookup::MdnsAddressLookup;

/// Wire the LAN mDNS address-lookup onto a bound endpoint. In iroh 1.0 the
/// provider is a companion crate built from the endpoint id and added to the
/// endpoint's address-lookup services.
pub(super) fn wire(endpoint: &Endpoint) -> Result<()> {
    let lookup = MdnsAddressLookup::builder()
        .build(endpoint.id())
        .context("building mDNS address-lookup")?;
    endpoint
        .address_lookup()
        .map_err(|err| anyhow::anyhow!("{err:?}"))
        .context("endpoint has no address-lookup services for mDNS")?
        .add(lookup);
    tracing::debug!("mDNS address-lookup wired (LAN multicast publish + resolve)");
    Ok(())
}
