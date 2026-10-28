use habilis_network::protocol::MeshId;

/// The `--text` / `--task-id` / `--label` sugar `a2a call` turns into a params
/// object when `--params` is absent.
pub(super) struct A2aCallSugar<'a> {
    pub(super) mesh: &'a MeshId,
    pub(super) text: Option<&'a str>,
    pub(super) task_id: Option<&'a crate::a2a::TaskId>,
    pub(super) label: Option<&'a str>,
}
