use super::advertise::{Advertiser, spawn_advertiser};
use super::config::{CreateConfig, JoinConfig, TopicConfig};
use super::error::{CreateError, JoinError};
use crate::output::Output;
use agent_habilis_mesh::protocol::{DirectorySelection, MeshConfig, resolve_lookups};
use agent_habilis_mesh::runtime::{
    CreateParams, EventLoopConfig, JoinParams, Resolved, TopicParams,
};
use agent_habilis_mesh::runtime::{SetupParams, setup_mesh};

/// Resolve + set up a create: the ready [`EventLoopConfig`] plus the spawned
/// directory advertiser task (if `advertise` was requested). The caller picks
/// the `output` sink (captured for [`MeshSession`], silent for the MCP core).
///
/// # Errors
/// [`CreateError::AdvertiseRequiresReachable`] / [`CreateError::Setup`].
pub(super) async fn create_setup(
    cfg: CreateConfig,
    output: Output,
) -> Result<
    (
        EventLoopConfig,
        crate::a2a::app::SurfacedIo,
        Option<Advertiser>,
    ),
    CreateError,
> {
    crate::register_build_version();
    let config = MeshConfig {
        lookups: resolve_lookups(cfg.public, cfg.lookups),
        password: None,
        issuer_pubkey: None,
    };
    // The advertiser reaches the directory over this mesh's own lookups.
    let directory_lookups = config.lookups.clone();
    // Map the api's `advertise: bool` + `directory` onto the shared
    // `DirectorySelection`; `resolve` validates it against the config
    // (loopback-only advertise is rejected before any setup work).
    let advertise = match (cfg.advertise, cfg.directory) {
        (false, _) => DirectorySelection::Unset,
        (true, None) => DirectorySelection::Default,
        (true, Some(directory)) => DirectorySelection::Named(directory),
    };
    let max_peers = cfg.max_peers;
    let Resolved {
        kind,
        author,
        advertise_directory,
    } = CreateParams {
        name: cfg.name,
        nickname: cfg.nickname,
        config,
        advertise,
        password: cfg
            .password
            .map(agent_habilis_mesh::protocol::Password::new),
        // Invite-only is a CLI-driven feature; the library api does not expose
        // it yet (a documented follow-up).
        invite_only: false,
    }
    .resolve()
    .map_err(|_| CreateError::AdvertiseRequiresReachable)?;
    let io = crate::a2a::app::SurfacedIo::new(output);
    let sink = io.sink();
    // See `cli::run_session`: the advertiser's counter predates setup so the
    // returned config needs no patching.
    let live_count = advertise_directory
        .as_ref()
        .map(|_| std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1)));
    let elc = setup_mesh(
        kind,
        SetupParams {
            author,
            max_peers,
            runtime_base: Some(crate::runtime_base()),
            state_file: None,
            sink,
            multihop: false,
            per_peer_gate: Some(crate::a2a::card_gate()),
            cohost: None,
            live_count: live_count.clone(),
        },
    )
    .await
    .map_err(|error| CreateError::Setup(error.context("setup_mesh failed")))?;
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
    cfg: JoinConfig,
    output: Output,
) -> Result<(EventLoopConfig, crate::a2a::app::SurfacedIo), JoinError> {
    let resolved = JoinParams {
        target: cfg.target,
        nickname: cfg.nickname,
        password: cfg
            .password
            .map(agent_habilis_mesh::protocol::Password::new),
    }
    .resolve()
    .map_err(JoinError::Resolve)?;
    resolved_setup(resolved, cfg.max_peers, output).await
}

/// Resolve + set up a topic (a string-derived public mesh).
///
/// # Errors
/// [`JoinError::Resolve`] if the string is empty/whitespace;
/// [`JoinError::Setup`] on endpoint/gossip failure.
pub(super) async fn topic_setup(
    cfg: TopicConfig,
    output: Output,
) -> Result<(EventLoopConfig, crate::a2a::app::SurfacedIo), JoinError> {
    let resolved = TopicParams {
        string: cfg.string,
        nickname: cfg.nickname,
    }
    .resolve()
    .map_err(JoinError::Resolve)?;
    resolved_setup(resolved, cfg.max_peers, output).await
}

/// The shared tail of [`join_setup`] / [`topic_setup`]: run `setup_mesh` for
/// an already-resolved join-flavored setup.
async fn resolved_setup(
    resolved: Resolved,
    max_peers: usize,
    output: Output,
) -> Result<(EventLoopConfig, crate::a2a::app::SurfacedIo), JoinError> {
    crate::register_build_version();
    let Resolved { kind, author, .. } = resolved;
    let io = crate::a2a::app::SurfacedIo::new(output);
    let sink = io.sink();
    let elc = setup_mesh(
        kind,
        SetupParams {
            author,
            max_peers,
            runtime_base: Some(crate::runtime_base()),
            state_file: None,
            sink,
            multihop: false,
            per_peer_gate: Some(crate::a2a::card_gate()),
            cohost: None,
            live_count: None,
        },
    )
    .await
    .map_err(|error| JoinError::Setup(error.context("setup_mesh failed")))?;
    Ok((elc, io))
}

/// The advertiser handle / signal-handling flag shared by
/// [`InProcessSession::spawn`] and [`MeshSession::with_events_and_signals`] —
/// the bits of the spawn call that are about the *process* environment rather
/// than the mesh itself.
pub(super) struct SpawnEnv {
    pub(super) advertiser: Option<Advertiser>,
    pub(super) handle_signals: bool,
}
