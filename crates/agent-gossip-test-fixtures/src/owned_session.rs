use std::ops::{Deref, DerefMut};
use std::path::Path;

use agent_gossip::api::{MeshSession, SessionError};

use crate::own_base::OwnBase;

/// A live session with the runtime base that it runs in.
///
/// The 2 are 1 value, so a test that takes the session out of its node takes the base with it, and the
/// base is removed only when the session is gone. It lends the session through `Deref`, so a call on
/// the field reads as a call on the session.
#[derive(Debug)]
pub struct OwnedSession {
    session: MeshSession,
    // After the session, so that the folder goes after it.
    base: OwnBase,
}

impl OwnedSession {
    pub(crate) fn new(session: MeshSession, base: OwnBase) -> Self {
        Self { session, base }
    }

    /// The folder that the session runs in.
    #[must_use]
    pub fn base(&self) -> &Path {
        self.base.as_path()
    }

    /// Leave the mesh, then remove the base.
    ///
    /// # Errors
    ///
    /// Returns the error of the session when its loop has already stopped.
    pub async fn leave(self) -> Result<(), SessionError> {
        let Self { session, base } = self;
        let outcome = session.leave().await;
        drop(base);
        outcome
    }
}

impl Deref for OwnedSession {
    type Target = MeshSession;

    fn deref(&self) -> &MeshSession {
        &self.session
    }
}

impl DerefMut for OwnedSession {
    fn deref_mut(&mut self) -> &mut MeshSession {
        &mut self.session
    }
}
