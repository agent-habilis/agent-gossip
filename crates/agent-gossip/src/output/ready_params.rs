use habilis_network::protocol::{MeshId, MeshName, Nickname};

/// The value cluster for [`Output::ready`]: the mesh identity fields plus
/// optional startup diagnostics (skill drift, `--a2a-serve` port).
#[derive(Clone, Copy)]
pub(crate) struct ReadyParams<'a> {
    pub mesh: &'a MeshId,
    pub name: &'a MeshName,
    pub nickname: &'a Nickname,
    pub drift: Option<&'a str>,
    pub a2a_port: Option<u16>,
}
