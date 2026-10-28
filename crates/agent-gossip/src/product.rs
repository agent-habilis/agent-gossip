/// This binary's name — the single place it is spelled for path purposes. The
/// engine takes it as a parameter (see [`habilis_network::util::runtime_base`])
/// rather than assuming it, because it is embedded by more than one binary.
pub(crate) const PRODUCT: &str = "agent-gossip";
