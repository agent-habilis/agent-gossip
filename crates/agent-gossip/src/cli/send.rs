use anyhow::Result;
use habilis_network::protocol::Nickname;
use habilis_network::runtime::ipc;
use habilis_network::runtime::ipc::{Addressed, IpcError};
use serde::Serialize;

/// [`habilis_network::runtime::ipc::send`] with this CLI's remedy appended.
///
/// The engine reports only the fact ([`IpcError::NoDaemon`]) — it has no commands to name
/// — so the "start one with …" half is composed here, beside the commands it
/// names. Rebuilt into a single message rather than an `anyhow` context layer:
/// context prepends, which would invert the fact-then-remedy order the operator
/// reads (and that `--output json`'s stderr contract pins).
pub(crate) async fn send<Payload>(command: &Payload, nickname: &Nickname) -> Result<String>
where
    Payload: Serialize + Addressed,
{
    ipc::send(&crate::runtime_base(), command, nickname)
        .await
        .map_err(|error| match error {
            IpcError::NoDaemon(no_daemon) => anyhow::anyhow!(
                "{no_daemon} Start one with `agent-gossip create` or \
                 `agent-gossip join {{hash}} --nickname {nickname}`."
            ),
            #[expect(
                clippy::wildcard_enum_match_arm,
                reason = "foreign non_exhaustive enum: the compiler needs a wildcard arm; the named variants are listed before it"
            )]
            other => anyhow::Error::from(other),
        })
}
