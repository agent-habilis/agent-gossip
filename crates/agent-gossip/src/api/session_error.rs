use std::fmt;

/// A request to a live mesh session failed. Each variant is one failure kind,
/// and its text is the one the CLI and the tests print.
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionError {
    /// The event loop stopped before it took the request.
    Stopped,
    /// The event loop took the request and dropped the reply channel.
    DroppedResponse,
    /// The event loop failed while it wound down on `leave`.
    LeaveFailed(anyhow::Error),
    /// The event loop answered the request with an error.
    Loop(anyhow::Error),
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => formatter.write_str("mesh event loop has stopped"),
            Self::DroppedResponse => formatter.write_str("mesh event loop dropped the response"),
            Self::LeaveFailed(error) | Self::Loop(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for SessionError {}
