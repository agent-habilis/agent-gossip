#[cfg(unix)]
use std::path::PathBuf;

use habilis_network::runtime::ipc;
use habilis_network::runtime::ipc::IpcError;
use serde::Serialize;

use crate::runtime_base;

/// The IPC endpoint of one live daemon on this machine: the socket path on
/// Unix, as before the port, and the pipe name that the marker holds on
/// Windows.
pub(crate) struct DaemonEndpoint {
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    name: ipc::IpcName,
}

impl DaemonEndpoint {
    /// Send one command and return the raw JSON reply.
    #[cfg(unix)]
    pub(crate) async fn send<Cmd: Serialize>(&self, command: &Cmd) -> Result<String, IpcError> {
        ipc::send_to_path(&self.path, command).await
    }

    /// Send one command and return the raw JSON reply.
    #[cfg(windows)]
    pub(crate) async fn send<Cmd: Serialize>(&self, command: &Cmd) -> Result<String, IpcError> {
        ipc::send_to_endpoint(&self.name, command).await
    }
}

/// Every daemon endpoint under the runtime base.
#[cfg(unix)]
pub(crate) fn active_endpoints() -> Vec<DaemonEndpoint> {
    let base = runtime_base();
    let paths = ipc::active_socket_paths(&base);
    paths.map(|path| DaemonEndpoint { path }).collect()
}

/// Every daemon endpoint under the runtime base.
#[cfg(windows)]
pub(crate) fn active_endpoints() -> Vec<DaemonEndpoint> {
    let base = runtime_base();
    let names = ipc::active_endpoints(&base);
    names.map(|name| DaemonEndpoint { name }).collect()
}
