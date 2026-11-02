use std::fmt;

use super::SessionError;

/// A directory session failed to open or to close. Each variant is one failure
/// kind, and its text is the one the CLI and the tests print.
#[derive(Debug)]
#[non_exhaustive]
pub enum DirectoryError {
    /// The directory name is not a valid mesh name.
    InvalidName(String),
    /// The directory mesh could not be joined.
    Join(anyhow::Error),
    /// The directory mesh did not shut down cleanly.
    Leave(SessionError),
}

impl fmt::Display for DirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(error) => write!(formatter, "invalid directory name: {error}"),
            Self::Join(error) => write!(formatter, "{error}"),
            Self::Leave(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for DirectoryError {}
