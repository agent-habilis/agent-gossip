use habilis_network::embed::EventLoopState;
use habilis_network::embed::HandlerCtx;
use habilis_network::protocol::Channel;
use tokio::time::Instant as TokioInstant;

use crate::a2a::app::A2aApp;

pub(crate) async fn tick(state: &mut EventLoopState, app: &mut A2aApp, ctx: &HandlerCtx<'_>) {
    let Some((bell, last_poll)) = app.surfaced.reader_to_publish(TokioInstant::now()) else {
        return;
    };
    let merge = serde_json::json!({
        "peers": { ctx.author.as_str(): { "reader": { "bell": bell, "last_poll": last_poll } } }
    });
    let published = habilis_network::ops::broadcast_state_merge(
        state,
        habilis_network::ops::StateMergeParams {
            mesh: ctx.mesh,
            author: ctx.author,
            merge,
            sender: ctx.sender,
            sink: ctx.sink,
            channel: Channel::Meta,
            surface: false,
        },
    )
    .await;
    if let Err(error) = published {
        tracing::debug!(target: "agent_gossip::a2a", %error, "reader liveness merge failed");
        app.surfaced.forget_published_reader();
    }
}
