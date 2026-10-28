use anyhow::Result;
use habilis_network::protocol::{MeshConfig, MeshName};
use habilis_network::runtime::CreateParams;

use super::args::CreateOpts;
use super::run_session::run_session;

/// Create a new mesh, print its identifier, and start listening.
pub(super) async fn create(options: CreateOpts) -> Result<()> {
    // Borrow-then-move: resolve the lookups and advertise selection (which
    // borrow `opts`) before moving `opts.name`/`opts.nickname` out.
    let advertise = options.advertise_selection();
    let password = super::password_flag::resolve_password(options.password.clone())?;
    // Naming no lookup is loopback and naming any restricts to it, except
    // that `--advertise` needs a lookup that reaches other machines.
    let lookups = super::args::lookup::lookups_or_public(
        &options.lookups.lookup,
        options.advertise.is_some(),
    );
    let mesh_parts = super::args::resolved::resolve(
        &lookups,
        options.lookups.relay_url.clone(),
        &options.transport,
    )?;
    let config = MeshConfig {
        lookups: mesh_parts.lookups,
        // The verifier is baked in at setup: its salt is the seed, which is
        // minted there. The flag's presence is all `resolve` needs.
        password: None,
        // Likewise the issuer pubkey: `set_invite` mints the keypair at setup
        // and bakes the pubkey; the `--invite-only` flag rides `CreateParams`.
        issuer_pubkey: None,
        transport: mesh_parts.transport,
        // `--no-gossip` is a mesh-wide characteristic baked into the id, so
        // every joiner inherits it from the ticket alone.
    };
    // `resolve` validates `--advertise` against the config (never a silent
    // no-op) before any setup work.
    let resolved = CreateParams {
        name: options.name.unwrap_or_else(MeshName::random),
        nickname: options.nickname,
        config,
        advertise,
        password,
        invite_only: options.invite_only,
    }
    .resolve()?;
    // Boxed: the session future carries the full `EventLoopConfig` and sits
    // just over clippy's `large_futures` budget (same reason the event loop
    // itself is boxed).
    Box::pin(run_session(resolved, options.shared)).await
}
