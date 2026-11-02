use clap::ValueEnum;

/// `doctor` is the one operator-facing report, so unlike every other command it
/// renders for a human by default and keeps the machine form behind `--output
/// json`. The agent commands stay JSON-only and keep the hidden
/// [`super::legacy_output::LegacyOutput`] no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    Human,
    Json,
}
