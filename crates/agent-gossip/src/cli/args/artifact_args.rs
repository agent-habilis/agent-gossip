use clap::Args;

use super::session_address::SessionAddress;

#[derive(Args, Debug)]
pub(crate) struct ArtifactArgs {
    #[command(flatten)]
    pub session: SessionAddress,
    /// The task id (uuid).
    #[arg(long)]
    pub task_id: crate::a2a::TaskId,
    /// The result text (optional when --file is given).
    #[arg(long)]
    pub text: Option<String>,
    /// Attach a file as the result, transferred peer-to-peer over the blob
    /// channel and referenced as a Part.url. For binaries too large to
    /// inline; the receiver fetches it with `agent-gossip a2a fetch <ticket>`.
    #[arg(long)]
    pub file: Option<std::path::PathBuf>,
    /// Filename to advertise for --file (defaults to the file's own name).
    #[arg(long, requires = "file")]
    pub file_name: Option<String>,
    /// MIME type to advertise for --file (e.g. application/pdf).
    #[arg(long, requires = "file")]
    pub file_mime: Option<String>,
}
