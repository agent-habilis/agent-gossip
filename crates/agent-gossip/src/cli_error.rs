use std::fmt;

/// The command-line run failed. Each command reports its own error text; this
/// type carries it up to the process exit.
#[derive(Debug)]
#[non_exhaustive]
pub enum CliError {
    /// A command failed; its error text is printed as is.
    Command(anyhow::Error),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for CliError {}
