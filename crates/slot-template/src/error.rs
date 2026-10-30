use std::fmt;
use std::path::PathBuf;

use crate::max_depth::MAX_DEPTH;

/// Why a template did not render. Each variant is one failure kind; its text is
/// the one the tests print.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
#[non_exhaustive]
pub enum Error {
    /// The template names a slot the call site did not provide.
    UnknownSlot {
        /// The slot name the template uses.
        name: String,
    },
    /// The call site passed an arg no `<!-- slot -->` in the template consumes.
    UnusedArg {
        /// The arg name the call site passed.
        name: String,
    },
    /// A comment starting `<!-- slot` that fails the directive grammar.
    MalformedSlot {
        /// Byte offset of the comment in the template.
        offset: usize,
        /// What is wrong with the directive.
        reason: String,
    },
    /// A comment starting `<!-- include` that fails the directive grammar
    /// (or is not alone on its own line).
    MalformedInclude {
        /// One-based line of the include directive.
        line: usize,
        /// What is wrong with the directive.
        reason: String,
    },
    /// An include chain revisits a file.
    IncludeCycle {
        /// The file the chain revisits.
        path: PathBuf,
    },
    /// An include chain nests deeper than `MAX_DEPTH`.
    DepthExceeded {
        /// The file at which the depth limit is hit.
        path: PathBuf,
    },
    /// The loader could not produce the file.
    Load {
        /// The file the loader failed to produce.
        path: PathBuf,
        /// Why the loader failed.
        reason: String,
    },
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
