use anyhow::Result;
use habilis_network::runtime::{
    Resolved, SetupKind, SetupParams, run as run_event_loop, setup_mesh,
};
use habilis_network::util::logging;

use super::args::SharedServerOpts;
use super::detach_if_owned::detach_if_owned;
use super::drift_warning::drift_warning;
use crate::a2a::a2a_app::{A2aApp, Startup, SurfacedIo};
use crate::a2a::a2a_binding;
use crate::api;
use crate::emission::{Output, OutputMode};

/// Build the output sink, set up the mesh, and run the event loop. The
/// shared spine of `create` and `join` — `resolved` carries the
/// already-resolved [`SetupKind`](habilis_network::runtime::SetupKind), author,
/// and advertise directory (see [`habilis_network::runtime::Resolved`]).
pub(super) async fn run_session(resolved: Resolved, shared: SharedServerOpts) -> Result<()> {
    detach_if_owned(&shared)?;
    let Resolved {
        kind,
        author,
        advertise_directory,
        ..
    } = resolved;
    // Refuse to share a state file with a daemon that is still using it.
    // Failing loudly here beats two daemons alternating ownership of one file
    // for the rest of the session, which is silent and corrupts identity
    // resolution for both. Checked before any network setup so the refusal
    // costs nothing.
    if let Some(path) = shared.state_file.as_ref()
        && let Some(owner) = super::session::live_owner_of(path).await
    {
        anyhow::bail!(
            "state file {} is already in use by {owner}. \
             A state file has one writer; sharing it makes both daemons \
             unreachable at random. Pass a different --state-file, or leave \
             that session first.",
            path.display(),
        );
    }
    // The advertiser reaches the directory over this mesh's own lookups
    // (only `create` advertises, so only `Create` carries them).
    let directory_lookups = if let SetupKind::Create { config, .. } = &kind {
        Some(config.lookups.clone())
    } else {
        None
    };
    let out = Output::new(OutputMode::Json, shared.filter_self);
    // Nag once at startup if an installed integration has fallen behind this
    // binary. CLI-only: the in-process paths pass `None` so in-process tests
    // stay hermetic. `agent-gossip status` is the on-demand counterpart.
    let drift = drift_warning();
    // The CLI owns the a2a application state. Tap its output so surfaced events
    // reach the app-side `poll`/`fetch` ring; the engine emits `NodeEvent`s
    // through `io.sink()` (`cfg.sink`) and the app's own `Output` render the
    // same tap.
    // `--a2a-serve`: bind the localhost JSON-RPC binding here (the app owns it).
    // Bound *before* the sink is built, because the resolved (possibly
    // ephemeral-assigned) port is one of the two startup diagnostics the sink
    // splices into `ready`; only handing the app its request channel has to
    // wait for the app to exist.
    let binding = match shared.a2a_serve {
        Some(port) => Some(a2a_binding::bind(port).await?),
        None => None,
    };
    let a2a_serve_port = binding.as_ref().map(|bound| bound.port);
    let io = SurfacedIo::new(out).set_startup(Startup {
        drift,
        a2a_port: a2a_serve_port,
    });
    let sink = io.sink();
    let mut app = A2aApp::new_with_io(io);
    let http_rx = binding.map(|bound| app.serve_a2a(bound));
    // Advertising needs a live-peer counter shared with the re-broadcast task.
    // Built before setup so the config is complete when it is returned, rather
    // than patched afterwards.
    let advertising = advertise_directory.as_ref().zip(directory_lookups.as_ref());
    let live_count = advertising.map(|_| api::initial_live_count());
    // The mesh mints its own endpoint and serves no extra ALPNs on its
    // Router: the A2A binding stands up a separate endpoint of its own
    // (see `tunnel::expose`), so there is no accept() loop to share.
    let mut setup_params = SetupParams::new(author, sink)
        .max_peers(shared.max_peers)
        .runtime_base(crate::runtime_base())
        .per_peer_gate(crate::a2a::card_gate());
    setup_params.state_file = shared.state_file;
    setup_params.live_count = live_count.clone();
    let config = setup_mesh(kind, setup_params).await?;
    let config = match shared.owner_pid {
        Some(owner) => config.set_owner_pid(owner)?,
        None => config,
    };
    // Advertising (`create --advertise`): start the re-broadcast task. It
    // reaches the directory over this mesh's own lookups. The handle is
    // held for the session's lifetime — on the CLI the process exits (via
    // signal) before it would drop, which tears the task down.
    let _advertiser = advertise_directory
        .zip(directory_lookups)
        .zip(live_count)
        .map(|((directory, lookups), counter)| {
            api::spawn_advertiser(&config, counter, directory, lookups)
        });
    // First point where mesh id + nickname are known — attach the
    // buffered log sink here (see `logging`).
    logging::attach(config.mesh_id(), config.author());
    // Boxed for the same reason `create` boxes the session future: it carries
    // the whole `EventLoopConfig` and sits just over clippy's `large_futures`
    // budget on 64-bit Linux (it fits under it on aarch64 macOS).
    Box::pin(run_event_loop(config, app, None, http_rx))
        .await
        .map_err(anyhow::Error::from)
}
