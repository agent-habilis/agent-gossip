use std::fmt;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::daemon::app::NodeDriver;
use crate::daemon::config::{DriverMode, EventLoopConfig};
use crate::protocol::mesh::MeshName;
use crate::protocol::{MeshId, Message, Nickname};
use crate::util::tuning::SESSION_REQUEST_CAP;

/// A live in-process membership over a background event loop, generic over the
/// application driver `A`. Drop it (or call [`Node::leave`]) to wind the loop
/// down.
pub struct Node<A: NodeDriver> {
    mesh_id: MeshId,
    name: MeshName,
    nickname: Nickname,
    req_tx: mpsc::Sender<A::Session>,
    quit_tx: mpsc::Sender<()>,
    task: Option<JoinHandle<anyhow::Result<()>>>,
}

// `A::Session` need not be `Debug`, and the channels/handle aren't useful in a
// log line — surface the identity fields a reader cares about.
impl<A: NodeDriver> fmt::Debug for Node<A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Node")
            .field("mesh_id", &self.mesh_id)
            .field("name", &self.name)
            .field("nickname", &self.nickname)
            .finish_non_exhaustive()
    }
}

impl<A: NodeDriver + 'static> Node<A> {
    /// Wire the in-process driver channels into `cfg`, spawn the event loop with
    /// `app`, and return the handle. `handle_signals` registers the process-wide
    /// ctrl-c / SIGTERM listeners (pass `true` for a foreground command that owns
    /// the process, `false` for a session nested inside one that does — see
    /// [`DriverMode::InProcess`]).
    ///
    /// `push` is the caller's inbound fan-out, or `None` for a consumer that
    /// drains frames some other way (a poll-only session, or an app that
    /// consumes every frame inside the loop). `None` is not merely a saved
    /// allocation: with a sender wired, the receive path calls
    /// `broadcast::Sender::receiver_count()` — which takes a mutex — on **every**
    /// inbound frame, only to discover nobody is listening.
    #[must_use]
    pub fn spawn(
        mut cfg: EventLoopConfig,
        app: A,
        push: Option<broadcast::Sender<Message>>,
        handle_signals: bool,
    ) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<A::Session>(SESSION_REQUEST_CAP);
        let (quit_tx, quit_rx) = mpsc::channel::<()>(1);
        cfg.driver = DriverMode::InProcess {
            msg_tx: push,
            quit_rx,
            handle_signals,
        };
        let mesh_id = cfg.mesh.clone();
        let name = cfg.name.clone();
        let nickname = cfg.author.clone();
        let task = tokio::spawn(crate::daemon::run(cfg, app, Some(req_rx), None));
        Self {
            mesh_id,
            name,
            nickname,
            req_tx,
            quit_tx,
            task: Some(task),
        }
    }

    /// The resolved mesh id.
    #[must_use]
    pub fn mesh_id(&self) -> &MeshId {
        &self.mesh_id
    }

    /// The mesh's human-readable name (decoded from the id).
    #[must_use]
    pub fn name(&self) -> &MeshName {
        &self.name
    }

    /// Our nickname in this mesh.
    #[must_use]
    pub fn nickname(&self) -> &Nickname {
        &self.nickname
    }

    /// Push one typed session request to the driver's
    /// [`handle_session`](NodeDriver::handle_session).
    ///
    /// # Errors
    /// Fails if the event loop has stopped (its receiver dropped).
    pub async fn send(&self, req: A::Session) -> anyhow::Result<()> {
        self.req_tx
            .send(req)
            .await
            .map_err(|_| anyhow::anyhow!("mesh event loop has stopped"))
    }

    /// Ask the loop to broadcast `Left` and wind down, waiting up to 3s. On
    /// timeout returns `Ok(())` and the task detaches.
    ///
    /// # Errors
    /// Returns an error if the event-loop task panicked or returned an error.
    pub async fn leave(mut self) -> anyhow::Result<()> {
        let _ = self.quit_tx.send(()).await;
        if let Some(task) = self.task.take() {
            let timeout = tokio::time::sleep(Duration::from_secs(3));
            tokio::select! {
                joined = task => {
                    joined
                        .map_err(|error| anyhow::anyhow!("mesh task panicked: {error}"))?
                        .map_err(|error| anyhow::anyhow!("mesh loop error: {error}"))?;
                }
                () = timeout => {}
            }
        }
        Ok(())
    }
}

impl<A: NodeDriver> Drop for Node<A> {
    fn drop(&mut self) {
        // Fallback if `leave()` was never called: abort the loop so it doesn't leak.
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
