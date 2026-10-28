/// This build's stamp: **this** crate's version plus the engine's git stamp,
/// e.g. `"0.7.2 (90103e5 dirty:false)"`. Every version surface reads it —
/// `--version`, `doctor`, the `ready` event, MCP `gossip_version`, and the A2A
/// card peers see — so they can never disagree.
///
/// Not the engine's ready-made `VERSION`, and not its `GIT_STAMP` either. Its
/// version leads with the *engine* crate's number, which shipped as a
/// stale-looking `0.5.0` on a `0.7.2` binary; and since the engine moved to the
/// separate `habilis-network` repo, its stamp names a commit that does not exist in this
/// history. Both halves are this crate's own: the number from `CARGO_PKG_VERSION`,
/// the stamp from `build.rs` (see `emit_git_stamp`).
#[must_use]
pub fn version() -> &'static str {
    concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("VERGEN_GIT_SHA"),
        " dirty:",
        env!("VERGEN_GIT_DIRTY"),
        ")"
    )
}
