//! The `--output` format value-enum — shared by every server command
//! (via `SharedServerOpts`) and by `poll` — plus its mapping to the
//! daemon's `OutputMode`.

use clap::ValueEnum;

use crate::output::OutputMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    Human,
    Json,
}

/// `OutputFormat` (the clap value-enum) → the daemon's `OutputMode`.
impl From<OutputFormat> for OutputMode {
    fn from(fmt: OutputFormat) -> Self {
        match fmt {
            OutputFormat::Human => OutputMode::Human,
            OutputFormat::Json => OutputMode::Json,
        }
    }
}
