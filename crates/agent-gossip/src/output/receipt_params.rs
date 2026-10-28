use habilis_network::protocol::Nickname;

/// The value cluster for [`Output::receipt`].
#[derive(Clone, Copy)]
pub(crate) struct ReceiptParams<'a> {
    pub(crate) from: &'a Nickname,
    pub(crate) ref_id: &'a str,
    pub(crate) state: &'a str,
    pub(crate) ts: u64,
}
