use habilis_network::protocol::message::Authored;

/// The value cluster for [`Output::state_changed`]: the channel, the patch
/// event, the freshly-derived document, and whether it was our own write.
#[derive(Clone, Copy)]
pub(crate) struct StateChangedParams<'a> {
    pub channel: habilis_network::protocol::Channel,
    pub event: &'a Authored,
    pub document: &'a serde_json::Value,
    pub is_self: bool,
}
