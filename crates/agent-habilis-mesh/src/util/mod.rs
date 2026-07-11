//! Small cross-cutting helpers shared across layers.

pub(crate) mod bounded_fifo_set;
pub(crate) mod bounded_queue;
pub(crate) mod bounded_read;
pub mod clock;
pub mod consts;
pub(crate) mod cooldown;
pub mod logs;
pub mod output;
pub mod process;
pub(crate) mod resident_memory;
pub mod tuning;
pub mod version;

/// The per-mesh folder name — the first 16 characters of the mesh
/// identifier. The stem of every per-mesh file (socket / log / state), so it
/// lives here rather than in any one module. See [`mesh_runtime_dir`].
///
/// The canonical id carries a `💬://` separator; the `://` is stripped first so
/// it never lands in a path (the `💬` sigil is filesystem-safe and kept, which
/// also keeps the stem identical to a legacy bare `💬<base58>` id).
#[must_use]
pub fn mesh_prefix(mesh_id: &str) -> String {
    mesh_id
        .replace(crate::protocol::mesh::SEPARATOR, "")
        .chars()
        .take(16)
        .collect()
}

/// The per-user runtime base — every per-mesh folder lives under it.
///
/// This replaces the former hardcoded, shared `/tmp/agent-square`: a single
/// world-traversable base let any *other* local user enumerate meshes and read
/// the (world-readable) per-member log files. Scoping the base to the user's
/// id — and creating it `0700` (see [`ensure_runtime_base`]) — closes that
/// cross-user exposure.
///
/// The path is a **fixed function of the uid alone**, deliberately *not* of the
/// environment: a daemon started in one context (say an interactive shell) and
/// a later `leave` / `session` / `msg` run in another (cron, a statusline
/// helper, `sudo`) must compute the *same* base or the CLI can no longer find
/// the daemon's socket and state file. That rules out `$XDG_RUNTIME_DIR`
/// (which varies across those contexts and would silently relocate everything)
/// and keeps us consistent with the project's "no env-var config" rule. The
/// `-<uid>` suffix adds only a few bytes over the old base, so the socket path
/// stays well inside the `AF_UNIX` `sun_path` ~104-byte limit.
#[must_use]
pub fn runtime_base() -> std::path::PathBuf {
    std::path::PathBuf::from(format!("/tmp/agent-square-{}", current_uid()))
}

/// This process's effective user id — the id a newly created file/dir is
/// *owned by*, so the id [`ensure_runtime_base`] must validate against (a
/// `getuid`/`geteuid` split under a setuid wrapper would otherwise reject a
/// base we just created ourselves). The one place the crate calls libc, so the
/// single `unsafe` block has one documented home (mirrors the termios FFI in
/// `main`). `geteuid` has no safety preconditions.
#[expect(unsafe_code, reason = "libc::geteuid FFI; no safe std wrapper exists")]
fn current_uid() -> u32 {
    // SAFETY: geteuid cannot fail and reads no memory.
    unsafe { libc::geteuid() }
}

/// Ensure [`runtime_base`] exists as a private (`0700`) directory this user
/// owns, tightening or rejecting a hostile pre-created one. Idempotent and
/// **run on every call, never cached** — a cached success would let a base
/// deleted mid-run (the OS `/tmp` reaper) silently reappear at `0755`.
///
/// Every writer that creates a file under the base must call this (directly, or
/// via [`ensure_mesh_runtime_dir`] / [`ensure_parent_private`]) **and honour
/// its error** first: skipping it lets an attacker who pre-creates the base as a
/// symlink redirect a `0600` state file (which carries the mesh id + A2A bearer
/// token) into an attacker-readable location. Callers whose target may be a
/// `--state-file` / `--log-dir` override (outside the base) should go through
/// [`ensure_parent_private`], which gates on [`is_under_runtime_base`].
///
/// # Errors
/// The base exists but is a symlink, is not a directory, or is owned by another
/// user (a squatting attempt — a clear message so the operator can remove it);
/// or the create / chmod syscalls fail. Fails *closed*: a failure here must
/// abort the write, never fall through to an unvalidated directory.
pub fn ensure_runtime_base() -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};

    // Deliberately *not* cached: a long-running daemon writes on every heartbeat,
    // and the OS `/tmp` reaper (systemd-tmpfiles, macOS periodic) can delete the
    // base mid-run. Re-creating + re-validating every call — a handful of cheap
    // syscalls — is what keeps a reaped base from silently reappearing at the
    // umask default `0755` on the next write. Idempotent and concurrency-safe:
    // racing creates collapse to `AlreadyExists`, the checks are read-only.
    let base = runtime_base();
    match std::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&base)
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    // `symlink_metadata` does not follow a link, so a base swapped for a symlink
    // into a world-readable dir is rejected rather than trusted.
    let meta = std::fs::symlink_metadata(&base)?;
    if !meta.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "runtime dir {} is not a directory — remove it",
                base.display()
            ),
        ));
    }
    if meta.uid() != current_uid() {
        // A local user squatted our predictable /tmp path. We refuse to write
        // secrets into a dir we do not own; the operator must clear it.
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "runtime dir {} is owned by another user (squatted) — remove it and retry",
                base.display()
            ),
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(base)
}

/// Whether `path` is inside [`runtime_base`]. A `--state-file` / `--log-dir`
/// override can point outside the base and must not be gated on its validation.
#[must_use]
pub(crate) fn is_under_runtime_base(path: &std::path::Path) -> bool {
    path.starts_with(runtime_base())
}

/// Prepare the parent directory of a file about to be written at `path`, with
/// the fail-closed base policy in one place: when `path` is under
/// [`runtime_base`], validate the private base first (aborting on a
/// squat/symlink) so `create_dir_all` never follows an attacker's redirect;
/// when it is an override outside the base, just create the parent. The single
/// home for the "gate on `is_under_runtime_base`, then ensure, then
/// `create_dir_all`" sequence — the token-bearing state file and the log sink
/// both route through here so the policy cannot drift between copies.
///
/// # Errors
/// [`ensure_runtime_base`] rejected the base, or the parent create failed.
pub(crate) fn ensure_parent_private(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    if is_under_runtime_base(path) {
        ensure_runtime_base()?;
    }
    if let Some(parent) = path.parent() {
        // Mode 0700 (not plain `create_dir_all`, which is 0755): closes the
        // narrow window where the reaper deletes the base between the ensure
        // above and this create, and keeps the per-mesh subdir private too.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(parent)?;
    }
    Ok(())
}

/// A mesh's runtime folder — `<runtime_base>/<mesh-prefix>/`. All of one
/// mesh's per-member files (`<nick>.tracing.log`, `<nick>.ipc.sock`,
/// `<nick>.state.json`) live here, so the socket, log, and state-file path
/// builders all derive from it. Pure — a path only; use
/// [`ensure_mesh_runtime_dir`] when about to *create* files under it.
#[must_use]
pub(crate) fn mesh_runtime_dir(mesh_id: &str) -> std::path::PathBuf {
    runtime_base().join(mesh_prefix(mesh_id))
}

/// Validate the private base (fail closed on a squat/symlink), then create the
/// per-mesh subdir. The choke point for writers whose target is *always* under
/// the base — the socket bind, the blob spool, the `.recv` dir. (The state file
/// and log sink can take a `--state-file` / `--log-dir` override outside the
/// base, so they route through [`ensure_parent_private`] instead.)
///
/// # Errors
/// [`ensure_runtime_base`] rejected the base, or the subdir create failed.
pub fn ensure_mesh_runtime_dir(mesh_id: &str) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::DirBuilderExt as _;
    ensure_runtime_base()?;
    let dir = mesh_runtime_dir(mesh_id);
    // 0700, and closes the reaper race (see `ensure_parent_private`).
    std::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::{is_under_runtime_base, mesh_prefix, runtime_base};
    use std::path::Path;

    #[test]
    fn runtime_base_is_deterministic_and_uid_scoped() {
        // Regression guard: the base must be a fixed function of the uid, not of
        // the environment — a daemon and a later CLI in a different context
        // (cron/sudo/statusline) have to agree, or the CLI can't find the socket.
        assert_eq!(runtime_base(), runtime_base());
        let base = runtime_base();
        let shown = base.to_string_lossy();
        assert!(
            shown.starts_with("/tmp/agent-square-"),
            "unexpected base: {shown}"
        );
    }

    #[test]
    fn is_under_runtime_base_discriminates() {
        assert!(is_under_runtime_base(
            &runtime_base().join("💬abc/nick.state.json")
        ));
        assert!(!is_under_runtime_base(Path::new("/etc/passwd")));
        assert!(!is_under_runtime_base(Path::new(
            "/tmp/agent-square/sessions/x.json"
        )));
    }

    #[test]
    fn truncates_to_16_chars() {
        assert_eq!(mesh_prefix("💬abcdefghijkmnpqrs").chars().count(), 16);
    }

    #[test]
    fn short_input_unchanged() {
        assert_eq!(mesh_prefix("💬abcd"), "💬abcd");
    }

    #[test]
    fn result_is_a_prefix_of_input() {
        let input = "💬abcdefghijkmnpqrstuvwx";
        assert!(input.starts_with(&mesh_prefix(input)));
    }

    #[test]
    fn strips_uri_separator_and_matches_legacy_stem() {
        // The `💬://` and legacy bare `💬` forms of the same id must produce an
        // identical, `/`-free filesystem stem.
        let uri = mesh_prefix("💬://abcdefghijkmnpqrs");
        let bare = mesh_prefix("💬abcdefghijkmnpqrs");
        assert_eq!(uri, bare);
        assert!(!uri.contains('/'));
    }
}
