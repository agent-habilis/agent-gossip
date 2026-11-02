use habilis_network::protocol::MeshId;

/// The `--text` / `--task-id` / `--label` sugar `a2a call` turns into a params
/// object when `--params` is absent.
pub(super) struct A2aCallSugar<'caller> {
    pub(super) mesh: &'caller MeshId,
    pub(super) text: Option<&'caller str>,
    pub(super) task_id: Option<&'caller crate::a2a::TaskId>,
    pub(super) label: Option<&'caller str>,
}
