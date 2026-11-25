use std::fs::DirBuilder;
use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use crate::counter::COUNTER;

/// The number of names that `OwnBase::new` tries before it gives up.
const MAX_TRIES: u32 = 64;

/// Make the folder with mode 0700 in one call, so no other user reaches it first. `false` says that the
/// name exists already.
fn create_new(path: &Path) -> std::io::Result<bool> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// A runtime base that one in-process node owns. Two nodes of one nickname on one host meet the host
/// lock when they share the base of the host, so each node gets a folder of its own. The folder name is
/// short because a Unix socket path has a length limit.
#[derive(Debug)]
pub(crate) struct OwnBase {
    path: PathBuf,
}

impl OwnBase {
    pub(crate) fn new() -> Self {
        let pid = std::process::id();
        // A name in the shared temp folder can exist already: an earlier run that crashed left it, or an other
        // user made it first. Such a folder is not ours, so each try takes the next number, and the count of
        // tries has a limit.
        let path = (0..MAX_TRIES)
            .map(|_| {
                let number = COUNTER.fetch_add(1, Ordering::Relaxed);
                std::env::temp_dir().join(format!("agp-{pid}-{number}"))
            })
            .find(|path| create_new(path).expect("the temp dir is writable"))
            .expect("a free folder name exists within the tries");
        Self { path }
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.path
    }
}

impl Drop for OwnBase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    use super::OwnBase;
    use crate::counter::COUNTER;

    /// A folder that a crashed run left, or that an other user made first in the shared temp folder,
    /// is not a base: its files are not ours, and its mode is not ours.
    #[test]
    fn a_base_is_never_a_folder_that_exists_already() {
        let next = COUNTER.load(Ordering::Relaxed);
        let pid = std::process::id();
        let taken: Vec<PathBuf> = (next..next + 20)
            .map(|number| std::env::temp_dir().join(format!("agp-{pid}-{number}")))
            .collect();
        for path in &taken {
            std::fs::create_dir_all(path).expect("the temp dir is writable");
        }

        let base = OwnBase::new();
        let mode = std::fs::metadata(base.as_path())
            .expect("`new` made the folder, so its mode can be read")
            .permissions()
            .mode()
            & 0o777;
        let reused = taken.contains(&base.path());
        for path in &taken {
            std::fs::remove_dir_all(path).ok();
        }

        assert!(!reused, "the base is a new folder");
        assert_eq!(mode, 0o700, "only the owner reaches the base");
    }
}
