use std::ops::{Deref, DerefMut};
use std::process::Child;

/// A child process that is killed and reaped when the test ends, even on a panic.
///
/// `Child` does neither on drop, so a test that panics before its explicit kill would leak the
/// process.
#[derive(Debug)]
pub struct KillOnDrop(Child);

impl KillOnDrop {
    /// Take ownership of `child`.
    #[must_use]
    pub fn new(child: Child) -> Self {
        Self(child)
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Deref for KillOnDrop {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.0
    }
}

impl DerefMut for KillOnDrop {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
