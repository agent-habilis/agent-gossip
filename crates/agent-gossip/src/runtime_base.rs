use crate::product::PRODUCT;

/// This product's per-user runtime base: `/tmp/agent-gossip-<uid>`.
///
/// A live filesystem contract — `skills/shared/daemon-session.md` hardcodes this
/// path, and every running daemon's socket and state file sit under it — so the
/// bytes must not change. Only `PRODUCT` feeds it.
#[must_use]
pub fn runtime_base() -> std::path::PathBuf {
    habilis_network::util::runtime_base(PRODUCT)
}
