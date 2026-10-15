use habilis_network::embed::EventLoopState;
use habilis_network::embed::HandlerCtx;

use crate::a2a::app::A2aApp;

pub(crate) async fn tick(
    _state: &mut EventLoopState,
    _app: &mut A2aApp,
    _ctx: &HandlerCtx<'_>,
) {
}
