use habilis_network::protocol::Nickname;

/// The value cluster for [`Output::receipt`].
#[derive(Clone, Copy)]
pub(crate) struct ReceiptParams<'caller> {
    pub(crate) from: &'caller Nickname,
    pub(crate) ref_id: &'caller str,
    pub(crate) state: &'caller str,
    pub(crate) ts: u64,
}
