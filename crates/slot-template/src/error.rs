use std::fmt;
use std::path::PathBuf;

use crate::max_depth::MAX_DEPTH;

#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Error {
    /// The template names a slot the call site did not provide.
    UnknownSlot { name: String },
    /// The call site passed an arg no `<!-- slot -->` in the template consumes.
    UnusedArg { name: String },
    /// A comment starting `<!-- slot` that fails the directive grammar.
    MalformedSlot { offset: usize, reason: String },
    /// A comment starting `<!-- include` that fails the directive grammar
    /// (or is not alone on its own line).
    MalformedInclude { line: usize, reason: String },
    /// An include chain revisits a file.
    IncludeCycle { path: PathBuf },
    /// An include chain nests deeper than [`MAX_DEPTH`].
    DepthExceeded { path: PathBuf },
    /// The loader could not produce the file.
    Load { path: PathBuf, reason: String },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnknownSlot { name } => {
                write!(formatter, "slot {name:?} has no arg providing it")
            }
            Error::UnusedArg { name } => {
                write!(formatter, "arg {name:?} matches no slot in the template")
            }
            Error::MalformedSlot { offset, reason } => {
                write!(
                    formatter,
                    "malformed slot directive at byte {offset}: {reason}"
                )
            }
            Error::MalformedInclude { line, reason } => {
                write!(
                    formatter,
                    "malformed include directive on line {line}: {reason}"
                )
            }
            Error::IncludeCycle { path } => {
                write!(formatter, "include cycle through {}", path.display())
            }
            Error::DepthExceeded { path } => {
                write!(
                    formatter,
                    "include depth exceeds {MAX_DEPTH} at {}",
                    path.display()
                )
            }
            Error::Load { path, reason } => {
                write!(formatter, "cannot load {}: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {}
