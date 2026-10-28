use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct TaskArtifactArgs {
    /// The task id (UUID) you're serving.
    pub(crate) task_id: String,
    /// The result text (optional when `file` is given).
    #[serde(default)]
    pub(crate) text: String,
    /// Optional local file path to attach as the result, transferred
    /// peer-to-peer over the blob channel (referenced as a Part.url). For
    /// binaries too large to inline; the initiator fetches it separately.
    #[serde(default)]
    pub(crate) file: Option<String>,
    /// Filename to advertise for `file` (defaults to the file's own name).
    #[serde(default)]
    pub(crate) file_name: Option<String>,
    /// MIME type to advertise for `file`.
    #[serde(default)]
    pub(crate) file_mime: Option<String>,
}
