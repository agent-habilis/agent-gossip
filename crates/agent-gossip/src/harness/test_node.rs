use std::path::PathBuf;

use habilis_network::protocol::AuthorKey;

use crate::api::{CreateConfig, CreateError, JoinConfig, JoinError, MeshSession, SessionError};

impl MeshSession {
    /// Create a mesh whose runtime files go to `base`, not to the runtime base of the host. A test gives
    /// each node a base of its own, so that two nodes of one nickname on one host do not meet the host
    /// lock. Test-only (the `test-node` feature): the public api has no such door.
    ///
    /// # Errors
    /// The same as [`MeshSession::create`].
    pub async fn create_in(config: CreateConfig, base: PathBuf) -> Result<Self, CreateError> {
        Self::create_with_base(config, base).await
    }

    /// Join a mesh whose runtime files go to `base`. Test-only, as [`MeshSession::create_in`].
    ///
    /// # Errors
    /// The same as [`MeshSession::join`].
    pub async fn join_in(config: JoinConfig, base: PathBuf) -> Result<Self, JoinError> {
        Self::join_with_base(config, base).await
    }

    /// The author key of this member, which a test needs when two members share a nickname and the
    /// nickname does not tell them apart. Test-only, as [`MeshSession::create_in`].
    ///
    /// # Errors
    /// Fails if the event loop has stopped.
    pub async fn own_key(&self) -> Result<AuthorKey, SessionError> {
        self.core.own_key().await
    }
}
