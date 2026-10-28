use agent_gossip::MeshName;

/// Validate a `&str` into a [`MeshName`] for the in-process harness.
pub(crate) fn test_mesh_name(name: &str) -> MeshName {
    MeshName::new(name).expect("valid test mesh name")
}
