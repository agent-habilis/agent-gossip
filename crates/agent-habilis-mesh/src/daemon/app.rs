use serde::de::DeserializeOwned;
use tokio::sync::oneshot;
use tokio::time::Instant as TokioInstant;

use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::daemon::state_file::StateFile;
use crate::gossip::app::NodeApp;
use crate::protocol::mesh::MeshName;

/// The application seam the daemon's event loop drives. A superset of
/// [`NodeApp`] (inbound frame classification/dispatch) that adds the app's own
/// timers, driver-channel inputs, and lifecycle hooks. The loop owns the mesh
/// [`EventLoopState`] and the concrete `impl NodeDriver`; every callback hands
/// the app the loop state plus the per-arm [`HandlerCtx`].
#[async_trait::async_trait]
pub trait NodeDriver: NodeApp {
    /// The typed in-process request an embed/MCP session pushes (the shared
    /// alternative to the CLI's IPC-over-socket path). Opaque to the engine.
    type Session: Send;
    /// One request from the localhost A2A JSON-RPC binding's HTTP task
    /// (`--a2a-serve`). Opaque to the engine.
    type Http: Send;
    /// One parsed IPC command off the unix-socket binding. The engine's generic
    /// `transport::ipc` socket server deserializes it (hence `DeserializeOwned`)
    /// and forwards it here; its variants are the app's a2a-typed command set,
    /// opaque to the engine.
    type Ipc: DeserializeOwned + Send + 'static;

    /// Seed the session state file with the app's discovery fields (the a2a
    /// bind port + bearer token). No-op when the app serves no local binding.
    ///
    /// Defaults to a no-op — an app that serves no local binding writes no
    /// discovery fields.
    fn init_state_file(&self, state_file: Option<&StateFile>) {
        let _ = state_file;
    }

    /// One-time startup hook, run once just before the select loop begins.
    ///
    /// Defaults to a no-op.
    async fn on_startup(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        let _ = (state, ctx);
    }

    /// The periodic app-maintenance tick that rides the membership-sweep
    /// cadence (its own per-item elapsed-time budgets gate the real work).
    ///
    /// Defaults to a no-op — an app with no periodic maintenance skips it.
    async fn on_tick(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        let _ = (state, ctx);
    }

    /// Graceful-shutdown hook: release app-owned resources and fail parked
    /// waiters. Runs after the engine has closed its own poll waiters, before
    /// the daemon removes the state file and broadcasts `Left`.
    ///
    /// Defaults to a no-op.
    async fn on_shutdown(&mut self, state: &mut EventLoopState) {
        let _ = state;
    }

    /// The earliest app-owned request deadline, for the loop's timer arm
    /// (`None` = the app is idle, so the arm pends forever).
    ///
    /// Defaults to `None` — an app with no request deadlines leaves the arm
    /// pending forever.
    fn earliest_deadline(&self) -> Option<TokioInstant> {
        None
    }

    /// Fail every app-owned request whose deadline has passed.
    ///
    /// Defaults to a no-op — pairs with the `None` [`Self::earliest_deadline`],
    /// which never fires the arm that would call this.
    fn expire_deadlines(&mut self, now: TokioInstant) {
        let _ = now;
    }

    /// Drain the app's surfaced-event tap into its `poll`/`fetch` ring and
    /// fulfill any long-poll waiter the new events advanced past. Runs at the
    /// bottom of every loop iteration (after the arm that produced the events),
    /// so a `poll` never misses an event surfaced in a prior iteration. The ring
    /// lives app-side because its element is the app's own surfaced-event type.
    ///
    /// Defaults to a no-op — an app that keeps no surfaced-event ring drains
    /// nothing.
    fn drain_surfaced(&mut self) {}

    /// The earliest parked long-poll waiter deadline, for the loop's
    /// `sleep_until_opt` arm (`None` = no waiters, so the arm pends forever).
    ///
    /// Defaults to `None` — an app with no long-poll waiters leaves the arm
    /// pending forever.
    fn earliest_poll_deadline(&self) -> Option<TokioInstant> {
        None
    }

    /// A long-poll deadline elapsed: fulfill any waiter a same-instant event
    /// made ready (so it wins over the timeout), then expire the rest.
    ///
    /// Defaults to a no-op — pairs with the `None` [`Self::earliest_poll_deadline`].
    fn poll_deadline_elapsed(&mut self) {}

    /// Empty every parked long-poll waiter with a clean timeout result — run on
    /// shutdown before the process may exit, so a held call never sees a
    /// dropped-channel error.
    ///
    /// Defaults to a no-op — an app with no parked waiters has none to close.
    fn close_poll_waiters(&mut self) {}

    /// Broadcast one line of interactive stdin (CLI chat).
    ///
    /// Defaults to a no-op — an app that isn't line-oriented ignores stdin.
    async fn handle_stdin(&mut self, line: &str, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        let _ = (line, state, ctx);
    }

    /// Dispatch one typed in-process session request. Returns `true` when it
    /// broadcast (so the loop refreshes the heartbeat-suppression clock).
    ///
    /// Defaults to ignoring the request and returning `false` — an app whose
    /// [`Self::Session`] is a trivial type takes no session requests.
    async fn handle_session(
        &mut self,
        req: Self::Session,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool {
        let _ = (req, state, ctx);
        false
    }

    /// Dispatch one localhost A2A JSON-RPC binding request against the live
    /// loop state, answering on its oneshot.
    ///
    /// Defaults to a no-op — an app whose [`Self::Http`] is a trivial type
    /// serves no localhost binding.
    async fn handle_http(
        &mut self,
        req: Self::Http,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) {
        let _ = (req, state, ctx);
    }

    /// Apply one parsed IPC command (the unix-socket `msg`/`poll`/… path),
    /// answering on `req.resp`. Returns `true` when it broadcast.
    ///
    /// Defaults to answering with a not-supported error and returning `false` —
    /// an app whose [`Self::Ipc`] is a trivial type serves no IPC command set.
    async fn handle_ipc(
        &mut self,
        req: IpcRequest<'_, Self::Ipc>,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool {
        let _ = (req.name, state, ctx);
        let _ = req
            .resp
            .send("{\"error\":\"this app serves no IPC commands\"}".to_owned());
        false
    }
}

/// One parsed IPC command bundled with its answer channel and this
/// session's mesh name — the value cluster [`NodeDriver::handle_ipc`] needs
/// beyond the loop state and handler context.
#[derive(Debug)]
pub struct IpcRequest<'a, C> {
    pub cmd: C,
    pub resp: oneshot::Sender<String>,
    pub name: &'a MeshName,
}
