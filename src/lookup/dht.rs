//! The mainline-DHT leg of the lookup layer: operator-free, eternal
//! publish/resolve of the seed-derived `rendezvous_id` via pkarr on the
//! `BitTorrent` mainline DHT. Wired only when `--dht` is in the allowlist;
//! it is the slow-but-always-there backstop and one of the
//! independently-sufficient reliability layers (see the lookup glossary
//! in AGENTS.md).

use anyhow::{Context, Result};
use iroh::Endpoint;
use iroh_mainline_address_lookup::DhtAddressLookup;

/// Wire the mainline-DHT address-lookup onto a bound endpoint. In iroh 1.0 the
/// provider is a companion crate added to the endpoint's address-lookup
/// services (it publishes/resolves via pkarr keyed on the endpoint's own key).
pub(super) fn wire(endpoint: &Endpoint) -> Result<()> {
    let lookup = DhtAddressLookup::builder()
        .build()
        .context("building mainline-DHT address-lookup")?;
    endpoint
        .address_lookup()
        .map_err(|err| anyhow::anyhow!("{err:?}"))
        .context("endpoint has no address-lookup services for DHT")?
        .add(lookup);
    tracing::debug!("mainline DHT address-lookup wired (pkarr publish + resolve)");
    Ok(())
}
