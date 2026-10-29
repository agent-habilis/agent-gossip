use habilis_network::protocol::{DirectorySelection, MeshConfig, resolve_lookups};
use habilis_network::runtime::{
    CreateParams, EventLoopConfig, JoinParams, Resolved, SetupParams, SignalHandling, setup_mesh,
};

use super::advertiser::{Advertiser, initial_live_count, spawn_advertiser};
use super::create_error::{CreateError, JoinError};
use super::join_config::{CreateConfig, JoinConfig, TopicConfig};
use crate::a2a::a2a_app::SurfacedIo;
use crate::emission::Output;
use crate::topic;

/// Resolve + set up a create: the ready [`EventLoopConfig`] plus the spawned
/// directory advertiser task (if `advertise` was requested). The caller picks
/// the `output` sink (captured for [`MeshSession`], silent for the MCP core).
///
/// # Errors
/// [`CreateError::AdvertiseRequiresReachable`] / [`CreateError::Setup`].
pub(super) async fn create_setup(
    config: CreateConfig,
    output: Output,
) -> Result<(EventLoopConfig, SurfacedIo, Option<Advertiser>), CreateError> {
    crate::register_build_version();
    let mesh_config = MeshConfig::new(
        // No sugar flag left in the api: naming no lookup is loopback,
        // naming any restricts to those.
        resolve_lookups(config.lookups),
        None,
        None,
        config.transport,
    );
    // The advertiser reaches the directory over this mesh's own lookups.
    let directory_lookups = mesh_config.lookups.clone();
    // Map the api's `advertise: bool` + `directory` onto the shared
    // `DirectorySelection`; `resolve` validates it against the config
    // (loopback-only advertise is rejected before any setup work).
    let advertise = match (config.advertise, config.directory) {
        (false, _) => DirectorySelection::Unset,
        (true, None) => DirectorySelection::Default,
        (true, Some(directory)) => DirectorySelection::Named(directory),
    };
    let max_peers = config.max_peers;
    let Resolved {
        kind,
        author,
        advertise_directory,
        ..
    } = CreateParams::new(config.name, config.nickname, mesh_config, advertise)
        // Invite-only is a CLI-driven feature; the library api does not expose
        // it yet (a documented follow-up).
        .with_password(
            config
                .password
                .map(habilis_network::protocol::Password::new),
        )
        .resolve()
        .map_err(CreateError::AdvertiseRequiresReachable)?;
    let io = SurfacedIo::new(output);
    let sink = io.sink();
    // See `cli::run_session`: the advertiser's counter predates setup so the
    // returned config needs no patching.
    let live_count = advertise_directory.as_ref().map(|_| initial_live_count());
    // The mesh mints its own endpoint and serves no extra ALPNs on its
    // Router: the A2A binding stands up a separate endpoint of its own
    // (see `tunnel::expose`), so there is no accept() loop to share.
    let mut setup_params = SetupParams::new(author, sink)
        .with_max_peers(max_peers)
        .with_runtime_base(crate::runtime_base())
        .with_per_peer_gate(crate::a2a::card_gate());
    setup_params.live_count = live_count.clone();
    let elc = setup_mesh(kind, setup_params).await.map_err(|error| {
        CreateError::Setup(anyhow::Error::from(error).context("setup_mesh failed"))
    })?;
    // When advertising, start the re-broadcast task (tied to this session);
    // it reaches the directory over this mesh's own lookups (moved into the
    // at-most-once closure, so no clone).
    let advertiser = advertise_directory
        .zip(live_count)
        .map(|(directory, counter)| spawn_advertiser(&elc, counter, directory, directory_lookups));
    Ok((elc, io, advertiser))
}

/// Resolve + set up a join: the ready [`EventLoopConfig`]. The caller picks
/// the `output` sink.
///
/// # Errors
/// [`JoinError::Resolve`] / [`JoinError::Setup`].
pub(super) async fn join_setup(
    config: JoinConfig,
    output: Output,
) -> Result<(EventLoopConfig, SurfacedIo), JoinError> {
    let resolved = JoinParams::new(
        config.target,
        config.nickname,
        config
            .password
            .map(habilis_network::protocol::Password::new),
    )
    .resolve()
    .map_err(|error| JoinError::Resolve(anyhow::Error::from(error)))?;
    resolved_setup(resolved, config.max_peers, output).await
}

/// Resolve + set up a topic (a string-derived public mesh).
///
/// # Errors
/// [`JoinError::Resolve`] if the string is empty/whitespace;
/// [`JoinError::Setup`] on endpoint/gossip failure.
pub(super) async fn topic_setup(
    config: TopicConfig,
    output: Output,
) -> Result<(EventLoopConfig, SurfacedIo), JoinError> {
    let resolved =
        topic::resolve_topic(config.string, config.nickname).map_err(JoinError::Resolve)?;
    resolved_setup(resolved, config.max_peers, output).await
}

/// The shared tail of [`join_setup`] / [`topic_setup`]: run `setup_mesh` for
/// an already-resolved join-flavored setup.
async fn resolved_setup(
    resolved: Resolved,
    max_peers: usize,
    output: Output,
) -> Result<(EventLoopConfig, SurfacedIo), JoinError> {
    crate::register_build_version();
    let Resolved { kind, author, .. } = resolved;
    let io = SurfacedIo::new(output);
    let sink = io.sink();
    // The mesh mints its own endpoint and serves no extra ALPNs on its
    // Router: the A2A binding stands up a separate endpoint of its own
    // (see `tunnel::expose`), so there is no accept() loop to share.
    let setup_params = SetupParams::new(author, sink)
        .with_max_peers(max_peers)
        .with_runtime_base(crate::runtime_base())
        .with_per_peer_gate(crate::a2a::card_gate());
    let elc = setup_mesh(kind, setup_params).await.map_err(|error| {
        JoinError::Setup(anyhow::Error::from(error).context("setup_mesh failed"))
    })?;
    Ok((elc, io))
}

/// The advertiser handle / signal-handling flag shared by
/// [`InProcessSession::spawn`] and [`MeshSession::new_with_events_and_signals`] —
/// the bits of the spawn call that are about the *process* environment rather
/// than the mesh itself.
pub(super) struct SpawnEnv {
    pub(super) advertiser: Option<Advertiser>,
    pub(super) handle_signals: SignalHandling,
}
