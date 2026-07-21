//! A2A-application state owned by the event loop, kept distinct from the
//! generic mesh state in [`agent_habilis_mesh::daemon::state::EventLoopState`]. Holds the
//! in-flight task registry, the outstanding gossip A2A-call waiters, and the
//! lazily-bound blob server — the pieces that belong to the a2a layer, not the
//! transport/membership engine. Threaded alongside `EventLoopState` as its own
//! `&mut` borrow.

use std::collections::HashMap;
use std::time::Instant;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant as TokioInstant;

use crate::a2a::TaskId;
use crate::a2a::surfaced::SurfacedState;
use crate::output::{Output, OutputEvent};
use agent_habilis_mesh::protocol::Nickname;

/// The tapped `Output` plus its surfaced-event receiver — the app's slice of the
/// daemon's surfacing plumbing, assembled by the caller (CLI / api / MCP) from
/// its base `Output` and handed to [`A2aApp::with_io`].
pub(crate) struct SurfacedIo {
    output: Output,
    surfaced_rx: UnboundedReceiver<OutputEvent>,
}

impl SurfacedIo {
    /// Tap `base` so every surfaced event is mirrored into the ring receiver.
    pub(crate) fn new(base: Output) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            output: base.with_tap(tx),
            surfaced_rx: rx,
        }
    }

    /// The engine sink for this tapped `Output`: a clone shared with the app's
    /// renderer, wrapped as a [`NodeSink`] so the engine emits `NodeEvent`s
    /// through the *same* tap the app's own `Output` writes to. Both feed the
    /// surfaced-events ring in surfacing order.
    pub(crate) fn sink(&self) -> std::sync::Arc<dyn agent_habilis_mesh::gossip::event::NodeSink> {
        std::sync::Arc::new(self.output.clone())
    }
}

/// All a2a-application state the event loop owns, split out of
/// `EventLoopState` so the mesh engine and the a2a layer keep disjoint state.
pub(crate) struct A2aApp {
    /// In-flight tasks this node is a party to, keyed by `task_id`
    /// (see [`crate::a2a::task`]). The coarse state machine + the two
    /// task timers (debounce sweep, ball-owner keepalive) read/write this;
    /// the skill owns the content. Third-party relays never insert here.
    pub tasks: HashMap<TaskId, crate::a2a::task::TaskRecord>,
    /// Outstanding gossip A2A RPC calls: an `A2aReq` was broadcast toward a
    /// peer and we're waiting for its `A2aResp` (matched by `rpc_id`) or the
    /// call's deadline. Fulfilled directly by the matching response frame.
    /// Bounded by [`POLL_WAITERS_CAP`](agent_habilis_mesh::util::consts::POLL_WAITERS_CAP).
    pub a2a_waiters: Vec<A2aWaiter>,
    /// The blob channel's serving endpoint + content-addressed store, bound
    /// lazily on the first large-file offload (an `a2a artifact`/`call --file`)
    /// and kept for the process lifetime so its address stays stable while we're
    /// alive to serve. `None` until the first offload; closed on shutdown.
    pub blob_server: Option<agent_habilis_mesh::blob::BlobServer>,
    /// The localhost A2A JSON-RPC binding's bound port + bearer token, set by
    /// [`serve_a2a`](Self::serve_a2a) when `--a2a-serve` is on. `None` (the
    /// default) means no local binding; the fields are written to the session
    /// state file (the local client's mode-600 discovery channel) at startup.
    a2a_port: Option<u16>,
    a2a_token: Option<String>,
    /// The a2a layer's concrete render sink (the tapped `Output`). Used for
    /// a2a-specific surfacings (`print_task` / `task_message` / `print_message`
    /// / `task_timeout`) the engine's generic `NodeSink` doesn't cover; generic
    /// events reach the same tap through `ctx.sink`.
    pub(crate) output: Output,
    /// The surfaced-events ring + long-poll waiters (the `poll`/`fetch`
    /// history). Held here so the daemon engine never names its element type.
    pub(crate) surfaced: SurfacedState,
    /// Receiver end of the `Output` tap, drained into [`surfaced`](Self::surfaced)
    /// each loop iteration. `None` for a detached test `A2aApp` (no live loop).
    pub(crate) surfaced_rx: Option<UnboundedReceiver<OutputEvent>>,
}

impl A2aApp {
    /// Build a detached a2a-application state (a silent sink, no surfacing tap)
    /// — for unit tests that drive handlers without a live event loop.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::detached(Output::silent())
    }

    /// Build the a2a-application state wired to a live surfacing tap: the tapped
    /// `Output` for a2a renders and the receiver drained into the ring.
    pub(crate) fn with_io(io: SurfacedIo) -> Self {
        let SurfacedIo {
            output,
            surfaced_rx,
        } = io;
        Self {
            tasks: HashMap::new(),
            a2a_waiters: Vec::new(),
            blob_server: None,
            a2a_port: None,
            a2a_token: None,
            output,
            surfaced: SurfacedState::new(),
            surfaced_rx: Some(surfaced_rx),
        }
    }

    /// A detached instance around an explicit `Output` (no tap), for tests /
    /// gossip-RPC fixtures that need an `A2aApp` without a running loop.
    #[cfg(test)]
    pub(crate) fn detached(output: Output) -> Self {
        Self {
            tasks: HashMap::new(),
            a2a_waiters: Vec::new(),
            blob_server: None,
            a2a_port: None,
            a2a_token: None,
            output,
            surfaced: SurfacedState::new(),
            surfaced_rx: None,
        }
    }

    /// Drain the `Output` tap into the ring, keeping only pollable events, then
    /// fulfill any long-poll waiter the new events advanced past.
    pub(crate) fn drain_surfaced_ring(&mut self) {
        let mut drained = Vec::new();
        if let Some(rx) = self.surfaced_rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                drained.push(event);
            }
        }
        for event in drained {
            if crate::a2a::node::is_pollable(&event) {
                self.surfaced.push(event);
            }
        }
        self.surfaced.fulfill_ready_poll_waiters();
    }

    /// Take ownership of an already-bound localhost A2A binding: record its
    /// port + bearer token (for the state file) and spawn the HTTP task,
    /// returning the request channel the event loop services. Called by the
    /// CLI when `--a2a-serve` is set.
    pub(crate) fn serve_a2a(
        &mut self,
        binding: crate::a2a::http::A2aBinding,
    ) -> tokio::sync::mpsc::Receiver<crate::a2a::rpc::A2aRequest> {
        self.a2a_port = Some(binding.port);
        self.a2a_token = Some(binding.token.clone());
        let (tx, rx) = tokio::sync::mpsc::channel::<crate::a2a::rpc::A2aRequest>(
            crate::a2a::http::REQUEST_QUEUE,
        );
        crate::a2a::http::spawn(binding, tx);
        rx
    }

    /// The a2a bind port + bearer token, if the local binding is on.
    pub(crate) fn a2a_discovery(&self) -> Option<(u16, &str)> {
        self.a2a_port.zip(self.a2a_token.as_deref())
    }

    /// The bound a2a port (`0` when no local binding), for gossip-RPC ops that
    /// echo it.
    pub(crate) fn a2a_port(&self) -> u16 {
        self.a2a_port.unwrap_or_default()
    }

    /// Register an outstanding gossip A2A call. Returns the responder back
    /// (unregistered) when the registry is at [`POLL_WAITERS_CAP`], so the
    /// caller can fail it fast instead of parking silently.
    #[must_use]
    pub(crate) fn register_a2a_waiter(&mut self, waiter: A2aWaiter) -> Option<A2aResponder> {
        if self.a2a_waiters.len() >= agent_habilis_mesh::util::consts::POLL_WAITERS_CAP {
            return Some(waiter.responder);
        }
        self.a2a_waiters.push(waiter);
        None
    }

    /// Resolve the waiter matching `corr` **from the peer we called** (`from`)
    /// with its JSON-RPC response `body`, and drop it. A response with no
    /// matching waiter, or one purporting to answer a call we made to a
    /// *different* peer (a forged reply with a guessed `corr`), is a no-op.
    pub(crate) fn fulfill_a2a_waiter(
        &mut self,
        corr: &agent_habilis_mesh::protocol::CorrId,
        from: &Nickname,
        body: &str,
    ) {
        if let Some(index) = self
            .a2a_waiters
            .iter()
            .position(|waiter| waiter.corr == *corr && waiter.peer == *from)
        {
            let waiter = self.a2a_waiters.swap_remove(index);
            waiter.responder.send_response(body);
        }
    }

    /// Whether we have an outstanding A2A call to `peer` correlated by `corr`
    /// — the gate that keeps an unsolicited/forged response from being acted on.
    pub(crate) fn has_a2a_waiter(
        &self,
        corr: &agent_habilis_mesh::protocol::CorrId,
        peer: &Nickname,
    ) -> bool {
        self.a2a_waiters
            .iter()
            .any(|waiter| waiter.corr == *corr && waiter.peer == *peer)
    }

    /// Adopt the authoritative `Task` a `SendMessage` returned (from `peer`, the
    /// worker) into our initiator-side registry. A `SendMessage` result is a
    /// `SendMessageResponse` (`{"task":…}`); a `GetTask` returns a bare `Task`.
    /// A non-Task response (a list, an error, a message echo) has no task and is
    /// ignored.
    pub(crate) fn adopt_returned_task(&mut self, peer: &Nickname, body: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return;
        };
        let result = &value["result"];
        // Unwrap the SendMessageResponse oneof if present, else treat the result
        // as a bare Task (GetTask). A v1.0 Task has no inline `kind`, so a Task
        // is recognized by its required `id` + `status.state`.
        let task = if result.get("task").is_some() {
            &result["task"]
        } else {
            result
        };
        let Some(task_id) = task["id"].as_str().and_then(TaskId::from_uuid_str) else {
            return;
        };
        let Some(task_state) = task["status"]["state"]
            .as_str()
            .and_then(|raw| serde_json::from_value(serde_json::Value::String(raw.to_owned())).ok())
        else {
            return;
        };
        crate::a2a::task::adopt_initiator(
            &mut self.tasks,
            crate::a2a::task::AdoptInitiatorParams {
                task_id: &task_id,
                peer,
                task_state,
                now: Instant::now(),
            },
        );
    }

    /// The earliest A2A-call deadline, for the loop's `sleep_until_opt` arm.
    pub(crate) fn earliest_a2a_deadline(&self) -> Option<TokioInstant> {
        self.a2a_waiters.iter().map(|waiter| waiter.deadline).min()
    }

    /// Time out every A2A call past its deadline.
    pub(crate) fn expire_a2a_waiters(&mut self, now: TokioInstant) {
        let survivors: Vec<A2aWaiter> = std::mem::take(&mut self.a2a_waiters)
            .into_iter()
            .filter_map(|waiter| {
                if waiter.deadline <= now {
                    waiter.responder.send_timeout();
                    None
                } else {
                    Some(waiter)
                }
            })
            .collect();
        self.a2a_waiters = survivors;
    }

    /// Time out every outstanding A2A call on shutdown.
    pub(crate) fn close_a2a_waiters(&mut self) {
        for waiter in std::mem::take(&mut self.a2a_waiters) {
            waiter.responder.send_timeout();
        }
    }

    /// Resolve every parked A2A call to `peer` with a peer-left error, now —
    /// the response can never arrive. The graceful-`Left` analogue of
    /// [`Self::expire_a2a_waiters`].
    pub(crate) fn fail_a2a_waiters_for_peer(&mut self, peer: &Nickname) {
        let survivors: Vec<A2aWaiter> = std::mem::take(&mut self.a2a_waiters)
            .into_iter()
            .filter_map(|waiter| {
                if waiter.peer == *peer {
                    waiter.responder.send_peer_left(peer);
                    None
                } else {
                    Some(waiter)
                }
            })
            .collect();
        self.a2a_waiters = survivors;
    }
}

/// How a fulfilled/expired gossip A2A call's response is delivered, per
/// transport: the CLI/IPC path wants the JSON-RPC response string to print;
/// the in-process path wants the parsed response `Value`; the localhost
/// JSON-RPC binding wants the `Result<Value, RpcError>` its HTTP handler
/// returns (so an external A2A client's `message/send` to a peer is served
/// over the same request/response waiter — task creation over the compliant
/// transport).
pub(crate) enum A2aResponder {
    Ipc(tokio::sync::oneshot::Sender<String>),
    Typed(tokio::sync::oneshot::Sender<serde_json::Value>),
    Rpc(tokio::sync::oneshot::Sender<Result<serde_json::Value, crate::a2a::rpc::RpcError>>),
}

impl A2aResponder {
    /// Deliver the peer's JSON-RPC response (`body` is the full
    /// `{"result"|"error"}` object string).
    pub(crate) fn send_response(self, body: &str) {
        match self {
            A2aResponder::Ipc(tx) => {
                let _ = tx.send(body.to_string());
            }
            A2aResponder::Typed(tx) => {
                let value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
                let _ = tx.send(value);
            }
            A2aResponder::Rpc(tx) => {
                let _ = tx.send(rpc_result_from_body(body));
            }
        }
    }

    /// Deliver a timeout (no response arrived before the deadline).
    pub(crate) fn send_timeout(self) {
        self.send_error("a2a request timed out (peer unreachable or slow)");
    }

    /// Deliver a peer-left error (the addressee broadcast a graceful `Left`,
    /// so no response can come).
    pub(crate) fn send_peer_left(self, peer: &Nickname) {
        self.send_error(&format!("a2a peer '{peer}' left the mesh"));
    }

    /// Fail the call with `-32000` and `message`, in each transport's shape.
    fn send_error(self, message: &str) {
        match self {
            A2aResponder::Ipc(tx) => {
                let _ = tx.send(
                    serde_json::json!({ "error": { "code": -32000, "message": message } })
                        .to_string(),
                );
            }
            A2aResponder::Typed(tx) => {
                let _ =
                    tx.send(serde_json::json!({ "error": { "code": -32000, "message": message } }));
            }
            A2aResponder::Rpc(tx) => {
                let _ = tx.send(Err(crate::a2a::rpc::RpcError {
                    code: -32000,
                    message: message.to_string(),
                }));
            }
        }
    }
}

/// Parse a peer's JSON-RPC response `body` (`{"result":…}` | `{"error":…}`)
/// into the `Result<Value, RpcError>` the localhost binding's HTTP handler
/// returns. A malformed/absent body reads as an internal error.
fn rpc_result_from_body(body: &str) -> Result<serde_json::Value, crate::a2a::rpc::RpcError> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Err(crate::a2a::rpc::RpcError {
            code: -32603,
            message: "peer returned a malformed a2a response".to_string(),
        });
    };
    if !value["error"].is_null() {
        return Err(crate::a2a::rpc::RpcError {
            code: value["error"]["code"].as_i64().unwrap_or(-32603),
            message: value["error"]["message"]
                .as_str()
                .unwrap_or("peer error")
                .to_string(),
        });
    }
    Ok(value["result"].clone())
}

/// An outstanding gossip A2A call, waiting for a response frame with a matching
/// correlation id `corr` from `peer`, or for `deadline` to elapse.
pub(crate) struct A2aWaiter {
    pub(crate) corr: agent_habilis_mesh::protocol::CorrId,
    pub(crate) peer: Nickname,
    pub(crate) deadline: TokioInstant,
    pub(crate) responder: A2aResponder,
}

#[cfg(test)]
mod tests {
    use super::{A2aResponder, rpc_result_from_body};
    use agent_habilis_mesh::protocol::Nickname;

    /// The localhost binding's `A2aResponder::Rpc` unwraps a peer's JSON-RPC
    /// response body into the `Result<Value, RpcError>` its HTTP handler
    /// returns: `result` → `Ok`, `error` → the peer's code/message, and a
    /// malformed/absent body → an internal error (never a panic).
    #[test]
    fn rpc_result_from_body_maps_result_error_and_garbage() {
        // A result comes back as Ok(the result value).
        let ok = rpc_result_from_body(r#"{"result":{"task":{"id":"t1"}}}"#)
            .expect("a result body is Ok");
        assert_eq!(ok["task"]["id"], "t1");

        // A JSON-RPC error carries the peer's code + message verbatim.
        let err = rpc_result_from_body(r#"{"error":{"code":-32602,"message":"unknown peer"}}"#)
            .expect_err("an error body is Err");
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "unknown peer");

        // Garbage is an internal error, not a panic.
        let garbage = rpc_result_from_body("not json").expect_err("garbage is Err");
        assert_eq!(garbage.code, -32603);
    }

    /// The `Rpc` responder delivers a fulfilled peer response and a deadline
    /// timeout to the localhost HTTP handler's oneshot as `Result<Value,
    /// RpcError>`.
    #[test]
    fn rpc_responder_delivers_response_and_timeout() {
        // A fulfilled response resolves to the parsed result.
        let (ok_tx, ok_rx) = tokio::sync::oneshot::channel();
        A2aResponder::Rpc(ok_tx).send_response(r#"{"result":{"task":{"id":"x"}}}"#);
        assert_eq!(ok_rx.blocking_recv().unwrap().unwrap()["task"]["id"], "x");

        // A deadline timeout resolves to the standard -32000 error.
        let (timeout_tx, timeout_rx) = tokio::sync::oneshot::channel();
        A2aResponder::Rpc(timeout_tx).send_timeout();
        assert_eq!(
            timeout_rx.blocking_recv().unwrap().unwrap_err().code,
            -32000
        );
    }

    /// A graceful `Left` resolves exactly the departed peer's parked calls —
    /// with a peer-left error, immediately — and leaves other peers' waiters
    /// parked for their own deadlines.
    #[test]
    fn peer_left_fails_only_that_peers_waiters() {
        let mut app = super::A2aApp::new();
        let far = tokio::time::Instant::now() + std::time::Duration::from_mins(5);
        let (bob_tx, bob_rx) = tokio::sync::oneshot::channel();
        let (carol_tx, carol_rx) = tokio::sync::oneshot::channel();
        app.a2a_waiters.push(super::A2aWaiter {
            corr: agent_habilis_mesh::protocol::CorrId::from("corr-bob"),
            peer: Nickname::from("bob"),
            deadline: far,
            responder: A2aResponder::Typed(bob_tx),
        });
        app.a2a_waiters.push(super::A2aWaiter {
            corr: agent_habilis_mesh::protocol::CorrId::from("corr-carol"),
            peer: Nickname::from("carol"),
            deadline: far,
            responder: A2aResponder::Typed(carol_tx),
        });

        app.fail_a2a_waiters_for_peer(&Nickname::from("bob"));

        let bob_response = bob_rx.blocking_recv().expect("bob's call resolves now");
        assert_eq!(bob_response["error"]["code"], -32000);
        assert_eq!(
            bob_response["error"]["message"],
            "a2a peer 'bob' left the mesh"
        );
        assert_eq!(app.a2a_waiters.len(), 1, "carol's waiter survives");
        assert_eq!(app.a2a_waiters[0].peer, Nickname::from("carol"));
        drop(app);
        assert!(
            carol_rx.blocking_recv().is_err(),
            "carol's waiter was never resolved, only dropped with the app"
        );
    }
}
