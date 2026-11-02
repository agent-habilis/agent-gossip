use habilis_network::protocol::MessageId;
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct MsgResponse {
    pub(super) ok: bool,
    pub(super) id: Option<MessageId>,
    pub(super) error: Option<String>,
}
