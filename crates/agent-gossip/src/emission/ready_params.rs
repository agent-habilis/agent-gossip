use habilis_network::protocol::{MeshId, MeshName, Nickname};

/// The value cluster for [`Output::ready`]: the mesh identity fields plus
/// optional startup diagnostics (skill drift, `--a2a-serve` port).
#[derive(Clone, Copy)]
pub(crate) struct ReadyParams<'caller> {
    pub mesh: &'caller MeshId,
    pub name: &'caller MeshName,
    pub nickname: &'caller Nickname,
    pub drift: Option<&'caller str>,
    pub a2a_port: Option<u16>,
}
