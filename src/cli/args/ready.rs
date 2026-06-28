//! `ready` command args: block until a backgrounded `create`/`join`
//! daemon reports — via its `--state-file` `ready` flag — that it is
//! serving, then exit. A gate (exit code only) for the CLI-polling
//! fallback: launch the daemon backgrounded, `ahs ready` on the same
//! `--state-file`, then `poll`. With `--output json` the gate also prints
//! `{swarm,name,nickname}`, so the caller need not parse the file itself.

use clap::Parser;

use super::output::OutputFormat;

#[derive(Parser, Debug)]
pub(crate) struct ReadyOpts {
    /// Path to the daemon's --state-file. Pass the SAME path you gave
    /// `create`/`join`. `ready` blocks until that file reports the daemon
    /// is serving, then exits 0 (non-zero on timeout).
    #[arg(long)]
    pub state_file: std::path::PathBuf,

    /// Max seconds to wait for the daemon to start serving before giving up.
    #[arg(long, default_value_t = crate::util::tuning::READY_MAX_SECS)]
    pub timeout_secs: u64,

    /// Output format. `human` (default) is a silent gate (exit code only);
    /// `json` prints `{swarm,name,nickname}` on success, so the gate doubles
    /// as the identity read.
    #[arg(long, default_value = "human")]
    pub output: OutputFormat,
}
