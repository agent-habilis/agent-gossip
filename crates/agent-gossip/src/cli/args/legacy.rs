//! Flags this CLI used to accept and now ignores.
//!
//! They are parsed (hidden from `--help`) rather than rejected on purpose: an
//! installed skill from an older generation still passes `--no-interactive` /
//! `--output json`, and its daemon launch line redirects **both** stdout and
//! stderr to `/dev/null` (see `skills/shared/daemon-session.md`). A clap
//! "unexpected argument" would exit 2 into that void — the daemon would never
//! start and the only symptom would be `agent-gossip ready` timing out with no
//! explanation. Accepting-and-ignoring keeps stale skills degrading to the
//! soft drift nag (`Output::ready`'s `drift` field) the way they were designed
//! to.

use clap::Parser;

/// The retired `--output` flag. Output is always JSON now.
#[derive(Parser, Debug)]
pub(crate) struct LegacyOutput {
    /// Deprecated no-op: every command emits JSON.
    #[arg(long, hide = true, value_name = "FORMAT")]
    pub output: Option<String>,
}
