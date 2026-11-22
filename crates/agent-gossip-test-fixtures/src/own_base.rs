use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crate::counter::COUNTER;

/// A runtime base that one in-process node owns. Two nodes of one nickname on one host meet the host
/// lock when they share the base of the host, so each node gets a folder of its own. The folder name is
/// short because a Unix socket path has a length limit.
#[derive(Debug)]
pub(crate) struct OwnBase {
    path: PathBuf,
}

impl OwnBase {
    pub(crate) fn new() -> Self {
        let number = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("agp-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&path).expect("the temp dir is writable");
        Self { path }
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.path.clone()
    }
}

impl Drop for OwnBase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
