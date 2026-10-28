use crate::runtime_base::runtime_base;

/// The per-mesh folder under [`runtime_base`], created after validating the base.
///
/// # Errors
/// As [`ensure_runtime_base`], or the subdir create failed.
pub fn ensure_mesh_runtime_dir(mesh_id: &str) -> std::io::Result<std::path::PathBuf> {
    habilis_network::util::ensure_mesh_runtime_dir(&runtime_base(), mesh_id)
}
