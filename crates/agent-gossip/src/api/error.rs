use std::fmt;

/// Why [`MeshSession::create`](super::MeshSession::create) failed, classified so callers can react:
/// the MCP server maps [`CreateError::AdvertiseRequiresReachable`] to an
/// `invalid_params` error and [`CreateError::Setup`] to an internal one.
#[derive(Debug)]
pub enum CreateError {
    /// `advertise` was requested on a loopback-only mesh — a directory
    /// listing requires a mesh reachable across machines.
    AdvertiseRequiresReachable,
    /// Endpoint / gossip / setup failure.
    Setup(anyhow::Error),
}

impl fmt::Display for CreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Single source of truth for the message: the validator's error.
            CreateError::AdvertiseRequiresReachable => {
                write!(
                    formatter,
                    "{}",
                    agent_habilis_mesh::protocol::AdvertiseRequiresReachable
                )
            }
            CreateError::Setup(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for CreateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CreateError::AdvertiseRequiresReachable => None,
            CreateError::Setup(error) => {
                let source: &(dyn std::error::Error + 'static) = error.as_ref();
                Some(source)
            }
        }
    }
}

/// Why [`MeshSession::join`](super::MeshSession::join) failed — the symmetric counterpart to
/// [`CreateError`]. `Resolve` is a malformed `💬…` id; `Setup` is an
/// endpoint/gossip failure. The MCP server maps both to an internal error.
#[derive(Debug)]
pub enum JoinError {
    /// The target could not be resolved into a mesh.
    Resolve(anyhow::Error),
    /// Endpoint / gossip / setup failure.
    Setup(anyhow::Error),
}

impl fmt::Display for JoinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JoinError::Resolve(error) | JoinError::Setup(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for JoinError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        let (JoinError::Resolve(error) | JoinError::Setup(error)) = self;
        let source: &(dyn std::error::Error + 'static) = error.as_ref();
        Some(source)
    }
}
