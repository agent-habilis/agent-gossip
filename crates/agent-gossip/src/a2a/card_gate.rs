/// The `meta` channel's per-peer write gate for the A2A data model: each peer
/// owns `peers.<nick>.card` and no one else may write it.
///
/// A function, not a `const`, only because [`SelfWriteGate`] holds `String`s.
/// One home so the pair cannot drift between the four setup paths that install
/// it — the engine needs it to plant the genesis entry and to refuse a forgery.
///
/// [`SelfWriteGate`]: habilis_network::embed::SelfWriteGate
pub(crate) fn card_gate() -> habilis_network::embed::SelfWriteGate {
    habilis_network::embed::SelfWriteGate::new("peers".to_owned(), "card")
}
