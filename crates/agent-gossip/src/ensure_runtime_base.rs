use crate::runtime_base::runtime_base;

/// [`runtime_base`], created and validated as a private (`0700`) directory this
/// user owns.
///
/// # Errors
/// The base is a symlink, is not a directory, is owned by another user, or the
/// create/chmod syscalls failed. See [`habilis_network::util::ensure_runtime_base`].
pub fn ensure_runtime_base() -> std::io::Result<std::path::PathBuf> {
    let base = runtime_base();
    habilis_network::util::ensure_runtime_base(&base)?;
    Ok(base)
}
