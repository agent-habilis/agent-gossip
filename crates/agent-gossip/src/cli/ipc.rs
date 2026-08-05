use anyhow::Result;
use serde::Serialize;

use fofoca::protocol::Nickname;
use fofoca::runtime::ipc::{Addressed, NoDaemon};

/// [`fofoca::runtime::ipc::send`] with this CLI's remedy appended.
///
/// The engine reports only the fact ([`NoDaemon`]) — it has no commands to name
/// — so the "start one with …" half is composed here, beside the commands it
/// names. Rebuilt into a single message rather than an `anyhow` context layer:
/// context prepends, which would invert the fact-then-remedy order the operator
/// reads (and that `--output json`'s stderr contract pins).
pub(crate) async fn send<C>(cmd: &C, nickname: &Nickname) -> Result<String>
where
    C: Serialize + Addressed,
{
    fofoca::runtime::ipc::send(&crate::runtime_base(), cmd, nickname)
        .await
        .map_err(|error| match error.downcast::<NoDaemon>() {
            Ok(no_daemon) => anyhow::anyhow!(
                "{no_daemon} Start one with `agent-gossip create` or \
                 `agent-gossip join {{hash}} --nickname {nickname}`."
            ),
            Err(other) => other,
        })
}
