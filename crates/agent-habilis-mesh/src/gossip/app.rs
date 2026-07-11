use crate::daemon::ctx::HandlerCtx;
use crate::daemon::state::EventLoopState;
use crate::protocol::{Message, Nickname};

/// Per-frame classification the engine needs to decide retention, push
/// surfacing, and fork/DAG indexing without knowing the app's tag taxonomy.
/// Only meaningful for `MessageKind::App` frames — the engine consults it
/// solely when [`crate::protocol::MessageKind::app_tag`] is `Some`.
#[expect(
    clippy::struct_excessive_bools,
    reason = "five independent per-frame wire policies the engine reads; grouping them into sub-enums would only obscure the flat classification"
)]
#[derive(Debug)]
pub struct AppClass {
    /// Goes in the chat message-log / poll-fetch buffer (vs. plumbing like an
    /// RPC request/response, which reaches its caller via a parked waiter).
    pub loggable: bool,
    /// A task liveness **beat** — never retained or surfaced via poll/fetch,
    /// only emitted as a progress widget event.
    pub beat: bool,
    /// The payload parses and agrees with its frame (id / addressing / role /
    /// correlation). A failing frame is dropped whole before it surfaces.
    pub valid: bool,
    /// Carries a per-author hash chain (`seq`/parents) and so participates in
    /// fork-detection + the cross-author DAG (broadcast chat).
    pub chained: bool,
    /// Does this frame's directed body arrive **sealed** (encrypted) to the
    /// addressee? When `true`, a directed frame addressed to us must unseal
    /// (decrypt) or be dropped — the a2a convention for its directed tags. When
    /// `false`, the directed body is plaintext and passes through unchanged (a
    /// non-a2a consumer's explicit choice). Only consulted on the addressee's
    /// directed path; ignored for broadcast/infra frames, which are never
    /// sealed.
    pub sealed: bool,
}

/// An inbound `App` frame plus the surfacing decision already made by the
/// engine's generic prefix (join-horizon / third-party gating), passed to
/// [`NodeApp::on_app_frame`].
#[derive(Debug)]
pub struct InboundApp<'a> {
    pub message: &'a Message,
    pub surfaceable: bool,
}

/// The seam the engine drives for application-payload handling. The engine
/// owns [`EventLoopState`] and hands the app the frame plus the loop context
/// once it has done parse → signature verify → mesh gate → dedup → shard
/// reassembly → unseal. The app never names an engine transport detail it
/// isn't given here.
#[async_trait::async_trait]
pub trait NodeApp: Send {
    /// Classify one inbound `App` frame by tag/payload — see [`AppClass`].
    fn classify(&self, message: &Message) -> AppClass;

    /// Dispatch an inbound `App` frame (addressed to us, or broadcast) after
    /// the engine's generic prefix. Returns `true` when the engine should
    /// proceed to retain/index the frame, `false` when it must stop (plumbing
    /// like an RPC leg, or a directed leg addressed elsewhere).
    async fn on_app_frame(
        &mut self,
        frame: InboundApp<'_>,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool;

    /// Surface a reassembled multipart `App` body through the same path an
    /// unsplit frame of that tag would take (the raw shards were already
    /// retained; this only surfaces the logical view).
    ///
    /// Defaults to a no-op — an app that never splits a body past
    /// [`crate::util::consts::MAX_MESSAGE_SIZE`] (so the engine never reassembles
    /// one for it) has nothing to surface here.
    fn surface_logical(&mut self, logical: &Message, surfaceable: bool, ctx: &HandlerCtx<'_>) {
        let _ = (logical, surfaceable, ctx);
    }

    /// Post-apply hook for a meta-channel event: adopt the author's published
    /// endpoint hint into the dial book, if any.
    ///
    /// Defaults to a no-op — an app that publishes no per-peer endpoint hint
    /// into the meta channel has nothing to adopt.
    fn on_meta_applied(
        &mut self,
        author: &Nickname,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) {
        let _ = (author, state, ctx);
    }

    /// The node just formed its first real-peer link (became meshed). Lets the
    /// app re-publish anything whose value depends on being meshed — e.g. its
    /// card's dial hint, now that the home relay is homed and the startup
    /// publish's (possibly path-less) hint can be corrected.
    ///
    /// Defaults to a no-op — an app with nothing to re-publish on the mesh edge
    /// leaves it empty.
    async fn on_meshed(&mut self, state: &mut EventLoopState, ctx: &HandlerCtx<'_>) {
        let _ = (state, ctx);
    }
}
