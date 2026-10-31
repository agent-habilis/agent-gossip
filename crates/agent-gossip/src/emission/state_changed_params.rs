use habilis_network::protocol::messages::Authored;

/// The value cluster for `Output::state_changed`: the channel, the patch
/// event, the freshly-derived document, and whether it was our own write.
#[derive(Clone, Copy)]
pub(crate) struct StateChangedParams<'caller> {
    pub channel: habilis_network::protocol::Channel,
    pub event: &'caller Authored,
    pub document: &'caller serde_json::Value,
    pub is_self: bool,
}
