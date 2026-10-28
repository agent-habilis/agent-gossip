use std::fmt;

/// Why a worker-pushed frame did not parse as the payload its kind promises.
/// Each variant's text is the one the logs and the tests print.
#[derive(Debug)]
#[non_exhaustive]
pub enum FramePayloadError {
    /// The frame's app tag is not the kind this parser reads.
    WrongKind(&'static str),
    /// The frame breaks a fixed rule of its kind (an extension, an id, a role).
    Shape(&'static str),
    /// A JSON payload failed to parse or serialize.
    Invalid {
        what: &'static str,
        source: serde_json::Error,
    },
    /// The message body was rejected by the engine.
    Body(String),
}

impl fmt::Display for FramePayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongKind(kind) => write!(formatter, "not an {kind} frame"),
            Self::Shape(rule) => formatter.write_str(rule),
            Self::Invalid { what, .. } => formatter.write_str(what),
            Self::Body(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for FramePayloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid { source, .. } => Some(source),
            Self::WrongKind(_) | Self::Shape(_) | Self::Body(_) => None,
        }
    }
}
