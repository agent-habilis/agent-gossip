use anyhow::Result;

use super::args::TopicOpts;
use super::run_session::run_session;
use crate::topic::resolve_topic;

/// Join a public mesh derived deterministically from a shared string. The
/// seed, name, and (always-public) config are all derived from the string, so
/// the same string joins the same topic on any machine — no id to share.
pub(super) async fn topic(options: TopicOpts) -> Result<()> {
    let resolved = resolve_topic(options.string, options.nickname)?;
    Box::pin(run_session(resolved, options.shared)).await
}
